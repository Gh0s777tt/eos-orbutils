extern crate event;
extern crate freedesktop_entry_parser;
extern crate libredox;
extern crate log;
extern crate orbclient;
extern crate orbfont;
extern crate redox_log;

use event::{user_data, EventQueue};
use libredox::data::TimeSpec;
use libredox::flag;
use log::{debug, error, info};
use redox_log::{OutputBuilder, RedoxLogger};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::{env, io, mem};

use orbclient::image::Image;
use orbclient::{Color, EventOption, Renderer, Window, WindowFlag, K_BKSP, K_ENTER, K_ESC};
use orbfont::Font;

use package::{IconSource, Package};
use theme::{BAR_COLOR, BAR_HIGHLIGHT_COLOR, TEXT_COLOR, TEXT_HIGHLIGHT_COLOR};

mod package;
mod theme;
mod ui;

pub use ui::*;

/// Draw the Start chooser: an optional search box (top-level menu only) followed
/// by the package rows. `search` carries the live query so the box shows what's
/// been typed; row `selected` is highlighted (its index is into `packages`, not
/// counting the search-box header).
/// The chooser row height, including the search-box header when present.
fn chooser_rows(packages_len: usize, search: bool) -> usize {
    (if search { 1 } else { 0 }) + packages_len
}

fn draw_chooser(
    window: &mut Window,
    font: &Font,
    packages: &mut Vec<Package>,
    selected: i32,
    search: Option<&str>,
) {
    let w = window.width();
    let isz = icon_small_size();

    window.set(BAR_COLOR);

    let mut y = 0;

    // Search box header (top-level Start menu): filter the list as you type.
    if let Some(query) = search {
        window.rect(0, y, w, isz as u32, BAR_HIGHLIGHT_COLOR);
        let label = if query.is_empty() {
            "Szukaj aplikacji…".to_string()
        } else {
            format!("Szukaj: {query}_  ({} wyników)", packages.len())
        };
        font.render(&label, font_size() as f32)
            .draw(window, isz + 8, y + 8, TEXT_HIGHLIGHT_COLOR);
        y += isz;
    }

    for (i, package) in packages.iter_mut().enumerate() {
        if i as i32 == selected {
            window.rect(0, y, w, isz as u32, BAR_HIGHLIGHT_COLOR);
        }

        let image = package.icon_small.image();
        window.image(0, y, image.width(), image.height(), image.data());

        font.render(&package.name, font_size() as f32).draw(
            window,
            isz + 8,
            y + 8,
            if i as i32 == selected {
                TEXT_HIGHLIGHT_COLOR
            } else {
                TEXT_COLOR
            },
        );

        y += isz;
    }

    window.sync();
}

/// The local timezone offset from UTC, in seconds east of UTC (positive = ahead).
/// Read from `/etc/tz-offset` (a plain integer — E-OS's simple, tz-database-free
/// form; a distinct path so it never clashes with Debian's zone-*name* file),
/// falling back to a numeric `TZ` env (`+02:00`, `-0500`, `+2`, `UTC+2`); 0 (UTC)
/// if unset. A fixed offset has no DST — a named-zone/DST clock waits on a real
/// tz database (tracked with per-machine identity at install).
fn tz_offset_secs() -> i64 {
    if let Ok(s) = std::fs::read_to_string("/etc/tz-offset") {
        if let Ok(v) = s.trim().parse::<i64>() {
            return v;
        }
    }
    if let Ok(tz) = std::env::var("TZ") {
        if let Some(v) = parse_tz_offset(tz.trim()) {
            return v;
        }
    }
    0
}

/// Parse a numeric UTC offset like `+02:00`, `-0500`, `+2` or `UTC+2` into seconds.
fn parse_tz_offset(s: &str) -> Option<i64> {
    let s = s.trim_start_matches("UTC").trim_start_matches("GMT");
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1_i64, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    if rest.is_empty() {
        return None;
    }
    let (hh, mm) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None if rest.len() >= 3 => rest.split_at(rest.len() - 2),
        None => (rest, "0"),
    };
    let h: i64 = hh.parse().ok()?;
    let m: i64 = mm.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// Break an absolute time (seconds since the Unix epoch) into a civil
/// `(year, month, day, hour, minute)`. Pure integer math — Howard Hinnant's
/// days-from-civil inverse — so the launcher needs no calendar crate.
fn civil_from_epoch(secs: i64) -> (i64, u32, u32, u32, u32) {
    let rem = secs.rem_euclid(86_400);
    let hour = (rem / 3600) as u32;
    let min = ((rem % 3600) / 60) as u32;
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // month-of-year, Mar=0 [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, hour, min)
}

struct Bar {
    children: Vec<(String, Child)>,
    packages: Vec<Package>,
    start: Option<Image>,
    tray: Vec<Option<Image>>,
    start_packages: Vec<Package>,
    category_packages: BTreeMap<String, Vec<Package>>,
    /// Every launchable app, flat — the pool the Start-menu search filters over.
    all_apps: Vec<Package>,
    font: Font,
    width: u32,
    height: u32,
    window: Window,
    selected: i32,
    selected_window: Window,
    time: String,
}

impl Bar {
    fn new(width: u32, height: u32) -> Bar {
        // E-OS: float the bar with side/bottom margins + rounded corners.
        let margin = (icon_size() / 2).max(10);
        let bar_width = width.saturating_sub((margin * 2) as u32);
        let all_packages = get_packages();
        // A flat copy of every app before the category split, for search.
        let all_apps = all_packages.clone();

        // Handle packages with categories
        let mut root_packages = Vec::new();
        let mut category_packages = BTreeMap::<String, Vec<Package>>::new();
        for package in all_packages {
            if package.categories.is_empty() {
                // Packages without a category go on the bar
                root_packages.push(package);
            } else {
                // Packages with a category are collected
                //TODO: since this clones the package, use an Arc to prevent icon reloads?
                for category in package.categories.iter() {
                    match category_packages.get_mut(category) {
                        Some(packages) => {
                            packages.push(package.clone());
                        }
                        None => {
                            category_packages.insert(category.clone(), vec![package.clone()]);
                        }
                    }
                }
            }
        }

        // Sort root packages by ID
        root_packages.sort_by(|a, b| a.id.cmp(&b.id));

        let mut start_packages = Vec::new();

        for (category, packages) in category_packages.iter_mut() {
            start_packages.push({
                let mut package = Package::new();
                package.name = category.to_string();
                let icon = format!("{}/icons/mimetypes/inode-directory.png", UI_PATH);
                package.icon.source = IconSource::Path(icon.clone().into());
                package.icon_small.source = IconSource::Path(icon.into());
                package.exec = format!("category={}", category);
                package
            });

            packages.push({
                let mut package = Package::new();
                package.name = "Go back".to_string();
                let icon = format!("{}/icons/mimetypes/inode-directory.png", UI_PATH);
                package.icon.source = IconSource::Path(icon.clone().into());
                package.icon_small.source = IconSource::Path(icon.into());
                package.exec = "exit".to_string();
                package
            });
        }

        start_packages.push({
            let mut package = Package::new();
            package.name = "Logout".to_string();
            let icon = format!("{}/icons/actions/system-log-out.png", UI_PATH);
            package.icon.source = IconSource::Path(icon.clone().into());
            package.icon_small.source = IconSource::Path(icon.into());
            package.exec = "exit".to_string();
            package
        });

        Bar {
            children: Vec::new(),
            packages: root_packages,
            start: load_icon(&format!("{}/icons/places/start-here.png", UI_PATH)),
            tray: vec![
                load_icon_small(&format!("{}/icons/status/tray-net.png", UI_PATH)),
                load_icon_small(&format!("{}/icons/status/tray-vol.png", UI_PATH)),
                load_icon_small(&format!("{}/icons/status/tray-set.png", UI_PATH)),
            ],
            start_packages,
            category_packages,
            all_apps,
            font: Font::find(Some("Sans"), None, None).unwrap(),
            width: bar_width,
            height,
            window: Window::new_flags(
                margin,
                height as i32 - icon_size() - margin,
                bar_width,
                icon_size() as u32,
                "",
                &[
                    WindowFlag::Async,
                    WindowFlag::Borderless,
                    WindowFlag::Transparent,
                ],
            )
            .expect("launcher: failed to open window"),
            selected: -1,
            selected_window: Window::new_flags(
                0,
                height as i32,
                width,
                (font_size() + 8) as u32,
                "",
                &[
                    WindowFlag::Async,
                    WindowFlag::Borderless,
                    WindowFlag::Transparent,
                ],
            )
            .expect("launcher: failed to open selected window"),
            time: String::new(),
        }
    }

    fn update_time(&mut self) {
        let time = libredox::call::clock_gettime(flag::CLOCK_REALTIME)
            .expect("launcher: failed to read time");

        // Shift UTC by the configured offset so the bar reads LOCAL time, then
        // break the epoch into a civil date (no chrono). Fixes the old bug where
        // the clock showed raw UTC (`ts % 86400`) with no date and no zone.
        let off = tz_offset_secs();
        let (y, mo, d, h, mi) = civil_from_epoch(time.tv_sec + off);
        let sign = if off < 0 { '-' } else { '+' };
        let (oh, om) = (off.abs() / 3600, (off.abs() % 3600) / 60);
        let zone = if om == 0 {
            format!("UTC{sign}{oh}")
        } else {
            format!("UTC{sign}{oh}:{om:02}")
        };
        self.time = format!("{y:04}-{mo:02}-{d:02}  {h:02}:{mi:02}  {zone}");
    }

    fn draw(&mut self) {
        // Floating rounded glass background (transparent outside the pill).
        self.window.set(Color::rgba(0, 0, 0, 0));
        self.window.rounded_rect(
            0,
            0,
            self.width,
            icon_size() as u32,
            (icon_size() / 2) as u32,
            true,
            BAR_COLOR,
        );

        let mut y = 0;

        // App icons flow from the left (indices 1..=n; index 0 is the centered Start).
        let mut x = 8;
        for (idx, package) in self.packages.iter_mut().enumerate() {
            let i = (idx + 1) as i32;
            let image = package.icon.image();
            if i == self.selected {
                self.window.rect(
                    x,
                    y,
                    image.width() as u32,
                    image.height() as u32,
                    BAR_HIGHLIGHT_COLOR,
                );

                self.selected_window.set(Color::rgba(0, 0, 0, 0));
                let text = self.font.render(&package.name, font_size() as f32);
                self.selected_window
                    .rect(0, 0, text.width() + 8, text.height() + 8, BAR_COLOR);
                text.draw(&mut self.selected_window, 4, 4, TEXT_HIGHLIGHT_COLOR);
                self.selected_window.sync();
                let sw_y = self.window.y() - self.selected_window.height() as i32 - 4;
                self.selected_window.set_pos(self.window.x() + x, sw_y);
            }

            self.window
                .image(x, y, image.width(), image.height(), image.data());

            let mut count = 0;
            for (exec, _) in self.children.iter() {
                if exec == &package.exec {
                    count += 1;
                }
            }
            if count > 0 {
                self.window
                    .rect(x + 4, y, image.width() - 8, 2, TEXT_HIGHLIGHT_COLOR);
            }

            x += image.width() as i32;
        }

        // Start button — centered (index 0): the E-OS diamond mark.
        if let Some(start) = self.start.as_ref() {
            let sx = (self.width as i32 - start.width() as i32) / 2;
            if 0 == self.selected {
                self.window.rect(
                    sx,
                    y,
                    start.width() as u32,
                    start.height() as u32,
                    BAR_HIGHLIGHT_COLOR,
                );
            }
            self.window
                .image(sx, y, start.width(), start.height(), start.data());
        }

        // Clock (far right): local date + time + zone, at 1× (the string is wider
        // than the old bare "HH:MM", so 2× would crowd the tray).
        let text = self.font.render(&self.time, font_size() as f32);
        x = self.width as i32 - text.width() as i32 - 12;
        y = (icon_size() - text.height() as i32) / 2;
        text.draw(&mut self.window, x, y, TEXT_HIGHLIGHT_COLOR);

        // System tray, right-to-left, to the left of the clock.
        let mut tx = x - 14;
        let ty = (icon_size() - icon_small_size()) / 2;
        for icon in self.tray.iter().rev().flatten() {
            tx -= icon.width() as i32;
            self.window
                .image(tx, ty, icon.width(), icon.height(), icon.data());
            tx -= 8;
        }

        self.window.sync();
    }

    fn start_window(&mut self, category_opt: Option<&String>) -> Option<String> {
        // Search only makes sense on the top-level Start menu; a category submenu
        // keeps its plain list. Work off owned clones so switching between the
        // category view and flat search results needs no borrow juggling.
        let searchable = category_opt.is_none();
        let level: Vec<Package> = match category_opt {
            Some(category) => self.category_packages.get(category)?.clone(),
            None => self.start_packages.clone(),
        };
        let all_apps: Vec<Package> = if searchable {
            self.all_apps.clone()
        } else {
            Vec::new()
        };

        // Empty query → the category/level list; non-empty → every app whose name
        // contains the query (case-insensitive).
        let build_view = |query: &str| -> Vec<Package> {
            if !searchable || query.is_empty() {
                level.clone()
            } else {
                let q = query.to_lowercase();
                all_apps
                    .iter()
                    .filter(|p| p.name.to_lowercase().contains(&q))
                    .cloned()
                    .collect()
            }
        };

        // The window is sized to exactly the current view (search box + rows) and
        // RECREATED whenever the query changes — resizing a live transparent window
        // mid-type left later results clipped, so a fresh, correctly-sized window is
        // drawn instead. `header_h` is the search-box row's height for hit-testing.
        let header_h = if searchable { icon_small_size() } else { 0 };
        let win_h = |n: usize| -> i32 {
            (chooser_rows(n, searchable).max(1) as i32) * icon_small_size()
        };

        let mut query = String::new();
        let mut view = build_view(&query);
        let mut h = win_h(view.len());
        let mut start_window = Window::new_flags(
            0,
            self.height as i32 - icon_size() - h,
            chooser_width(),
            h as u32,
            "Start",
            &[WindowFlag::Borderless, WindowFlag::Transparent],
        )
        .unwrap();

        let mut selected = -1;
        let mut mouse_y = 0;
        let mut mouse_left = false;
        let mut last_mouse_left = false;
        draw_chooser(
            &mut start_window,
            &self.font,
            &mut view,
            selected,
            if searchable { Some(query.as_str()) } else { None },
        );
        'start_choosing: loop {
            let mut relayout = false;
            // Drain this batch into a Vec so `start_window` isn't borrowed while it
            // may need to be recreated below after the query changes.
            let events: Vec<_> = start_window.events().collect();
            for event in events {
                let redraw = match event.to_option() {
                    EventOption::Mouse(mouse_event) => {
                        mouse_y = mouse_event.y;
                        true
                    }
                    EventOption::Button(button_event) => {
                        mouse_left = button_event.left;
                        true
                    }
                    EventOption::Key(key_event) if key_event.pressed => {
                        match key_event.scancode {
                            K_ESC => break 'start_choosing,
                            K_BKSP if searchable => {
                                query.pop();
                                relayout = true;
                            }
                            K_ENTER if searchable => {
                                // Launch the highlighted result, else the first.
                                let idx = if selected >= 0 { selected as usize } else { 0 };
                                if let Some(p) = view.get(idx) {
                                    return Some(p.exec.to_string());
                                }
                            }
                            _ => {}
                        }
                        false
                    }
                    // Orbital delivers printable characters as a separate TextInput
                    // event (a Key event carries only the scancode), so the search
                    // query is fed from here, not from key_event.character.
                    EventOption::TextInput(text_input) if searchable => {
                        let c = text_input.character;
                        if !c.is_control() && c != '\0' {
                            query.push(c);
                            relayout = true;
                        }
                        false
                    }
                    EventOption::Focus(focus_event) => {
                        if !focus_event.focused {
                            break 'start_choosing;
                        } else {
                            false
                        }
                    }
                    EventOption::Quit(_) => break 'start_choosing,
                    _ => false,
                };

                if redraw && !relayout {
                    // Rows begin below the (top-anchored) search-box header.
                    let mut now_selected = -1;
                    let mut y = header_h;
                    for j in 0..view.len() {
                        if mouse_y >= y && mouse_y < y + icon_small_size() {
                            now_selected = j as i32;
                        }
                        y += icon_small_size();
                    }

                    if now_selected != selected {
                        selected = now_selected;
                        draw_chooser(
                            &mut start_window,
                            &self.font,
                            &mut view,
                            selected,
                            if searchable { Some(query.as_str()) } else { None },
                        );
                    }

                    if mouse_left && !last_mouse_left {
                        let mut y = header_h;
                        for package_i in 0..view.len() {
                            if mouse_y >= y && mouse_y < y + icon_small_size() {
                                return Some(view[package_i].exec.to_string());
                            }
                            y += icon_small_size();
                        }
                    }

                    last_mouse_left = mouse_left;
                }
            }

            // Query changed: rebuild the view and recreate the window at the new
            // size (the events borrow is released now).
            if relayout {
                view = build_view(&query);
                selected = -1;
                h = win_h(view.len());
                start_window = Window::new_flags(
                    0,
                    self.height as i32 - icon_size() - h,
                    chooser_width(),
                    h as u32,
                    "Start",
                    &[WindowFlag::Borderless, WindowFlag::Transparent],
                )
                .unwrap();
                draw_chooser(
                    &mut start_window,
                    &self.font,
                    &mut view,
                    selected,
                    if searchable { Some(query.as_str()) } else { None },
                );
            }
        }
        None
    }

    fn spawn(&mut self, exec: String) {
        match exec_to_command(&exec, None) {
            Some(mut command) => match command.spawn() {
                Ok(child) => {
                    self.children.push((exec, child));
                    //TODO: should redraw be done here?
                    self.draw();
                }
                Err(err) => error!("failed to spawn {}: {}", exec, err),
            },
            None => error!("failed to parse {}", exec),
        }
    }
}

fn bar_main(width: u32, height: u32) -> io::Result<()> {
    let mut bar = Bar::new(width, height);

    // The desktop icon layer goes first: wait for its DESKTOP-READY line so its
    // Back window exists before the wallpaper's (orbital keeps creation order
    // among Back windows — earlier = closer to the viewer). Both stay direct
    // children of the bar so logout kills them.
    let mut _desktop_stdout = None;
    match Command::new("desktop").stdout(Stdio::piped()).spawn() {
        Ok(mut child) => {
            if let Some(stdout) = child.stdout.take() {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            if line.contains("DESKTOP-READY") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // keep the pipe open for the session: desktop logs to stdout
                // and a closed pipe would turn its prints into panics
                _desktop_stdout = Some(reader.into_inner());
            }
            bar.children.push(("desktop".to_string(), child));
        }
        Err(err) => error!("failed to launch desktop: {}", err),
    }

    match Command::new("background").spawn() {
        Ok(child) => bar.children.push(("background".to_string(), child)),
        Err(err) => error!("failed to launch background: {}", err),
    }

    user_data! {
        enum Event {
            Time,
            Window,
        }
    }
    let event_queue = EventQueue::<Event>::new().expect("launcher: failed to create event queue");

    let mut time_file = File::open(&format!("/scheme/time/{}", flag::CLOCK_MONOTONIC))?;

    event_queue
        .subscribe(
            time_file.as_raw_fd() as usize,
            Event::Time,
            event::EventFlags::READ,
        )
        .expect("launcher: failed to subscribe to timer");
    event_queue
        .subscribe(
            bar.window.as_raw_fd() as usize,
            Event::Window,
            event::EventFlags::READ,
        )
        .expect("launcher: failed to subscribe to timer");

    let mut mouse_x = -1;
    let mut mouse_y = -1;
    let mut mouse_left = false;
    let mut last_mouse_left = false;

    let all_events = [Event::Time, Event::Window].into_iter();

    'events: for event in all_events
        .chain(event_queue.map(|e| e.expect("launcher: failed to get next event").user_data))
    {
        match event {
            Event::Time => {
                let mut time_buf = [0_u8; core::mem::size_of::<TimeSpec>()];
                if time_file.read(&mut time_buf)? < mem::size_of::<TimeSpec>() {
                    continue;
                }

                let mut i = 0;
                while i < bar.children.len() {
                    let remove = match bar.children[i].1.try_wait() {
                        Ok(None) => false,
                        Ok(Some(status)) => {
                            info!(
                                "{} ({}) exited with {}",
                                bar.children[i].0,
                                bar.children[i].1.id(),
                                status
                            );
                            true
                        }
                        Err(err) => {
                            error!(
                                "failed to wait for {} ({}): {}",
                                bar.children[i].0,
                                bar.children[i].1.id(),
                                err
                            );
                            true
                        }
                    };
                    if remove {
                        bar.children.remove(i);
                    } else {
                        i += 1;
                    }
                }

                loop {
                    let mut status = 0;
                    let pid = wait(&mut status)?;
                    if pid == 0 {
                        break;
                    }
                }

                bar.update_time();
                bar.draw();

                match libredox::data::timespec_from_mut_bytes(&mut time_buf) {
                    time => {
                        time.tv_sec += 1;
                        time.tv_nsec = 0;
                    }
                }
                time_file.write(&time_buf)?;
            }
            Event::Window => {
                for event in bar.window.events() {
                    //TODO: remove hack for super event
                    if event.code >= 0x1000_0000 {
                        let mut super_event = event;
                        super_event.code -= 0x1000_0000;

                        //TODO: configure super keybindings
                        let event_option = super_event.to_option();
                        debug!("launcher: super {:?}", event_option);
                        match event_option {
                            EventOption::Key(key_event) => match key_event.scancode {
                                orbclient::K_B => {
                                    if key_event.pressed {
                                        bar.spawn("netsurf-fb".to_string());
                                    }
                                }
                                orbclient::K_F => {
                                    if key_event.pressed {
                                        bar.spawn("cosmic-files".to_string());
                                    }
                                }
                                orbclient::K_T => {
                                    if key_event.pressed {
                                        bar.spawn("cosmic-term".to_string());
                                    }
                                }
                                _ => (),
                            },
                            _ => (),
                        }

                        continue;
                    }

                    let redraw = match event.to_option() {
                        EventOption::Mouse(mouse_event) => {
                            mouse_x = mouse_event.x;
                            mouse_y = mouse_event.y;
                            true
                        }
                        EventOption::Button(button_event) => {
                            mouse_left = button_event.left;
                            true
                        }
                        EventOption::Screen(screen_event) => {
                            bar.width = screen_event.width;
                            bar.height = screen_event.height;
                            bar.window
                                .set_pos(0, screen_event.height as i32 - icon_size());
                            bar.window.set_size(screen_event.width, icon_size() as u32);
                            bar.selected = -2; // Force bar redraw
                            bar.selected_window.set_pos(0, screen_event.height as i32);
                            bar.selected_window
                                .set_size(screen_event.width, (font_size() + 8) as u32);
                            true
                        }
                        EventOption::Hover(hover_event) => {
                            if hover_event.entered {
                                false
                            } else {
                                mouse_x = -1;
                                mouse_y = -1;
                                true
                            }
                        }
                        EventOption::Quit(_) => break 'events,
                        _ => false,
                    };

                    if redraw {
                        let mut now_selected = -1;

                        {
                            let y = 0;
                            // App icons from the left (indices 1..=n).
                            let mut x = 8;
                            for (idx, package) in bar.packages.iter_mut().enumerate() {
                                let i = (idx + 1) as i32;
                                let image = package.icon.image();
                                if mouse_y >= y
                                    && mouse_x >= x
                                    && mouse_x < x + image.width() as i32
                                {
                                    now_selected = i;
                                }
                                x += image.width() as i32;
                            }
                            // Centered Start (index 0).
                            if let Some(start) = bar.start.as_ref() {
                                let sx = (bar.width as i32 - start.width() as i32) / 2;
                                if mouse_y >= y
                                    && mouse_x >= sx
                                    && mouse_x < sx + start.width() as i32
                                {
                                    now_selected = 0;
                                }
                            }
                        }

                        if now_selected != bar.selected {
                            bar.selected = now_selected;
                            let sw_y = bar.height as i32;
                            bar.selected_window.set_pos(0, sw_y);
                            bar.draw();
                        }

                        if mouse_left && !last_mouse_left {
                            let mut i = 0;

                            if i == bar.selected {
                                let mut category_opt = None;
                                while let Some(exec) = bar.start_window(category_opt.as_ref()) {
                                    if exec.starts_with("category=") {
                                        let category = &exec[9..];
                                        category_opt = Some(category.to_string());
                                    } else if exec == "exit" {
                                        if category_opt.is_some() {
                                            category_opt = None;
                                        } else {
                                            break 'events;
                                        }
                                    } else {
                                        bar.spawn(exec);
                                        break;
                                    }
                                }
                            }
                            i += 1;

                            for package_i in 0..bar.packages.len() {
                                if i == bar.selected {
                                    let exec = bar.packages[package_i].exec.clone();
                                    bar.spawn(exec);
                                }
                                i += 1;
                            }
                        }

                        last_mouse_left = mouse_left;
                    }
                }
            }
        }
    }

    debug!("Launcher exiting, killing {} children", bar.children.len());
    for (exec, child) in bar.children.iter_mut() {
        let pid = child.id();
        match child.kill() {
            Ok(()) => debug!("Successfully killed child: {}", pid),
            Err(err) => error!("failed to kill {} ({}): {}", exec, pid, err),
        }
        match child.wait() {
            Ok(status) => info!("{} ({}) exited with {}", exec, pid, status),
            Err(err) => error!("failed to wait for {} ({}): {}", exec, pid, err),
        }
    }

    // kill any descendents of one of the children killed above that are still running
    debug!("Launcher exiting, reaping all zombie processes");
    let mut status = 0;
    loop {
        match wait(&mut status) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }

    Ok(())
}

fn chooser_main(paths: env::Args) {
    for ref path in paths.skip(1) {
        let mut packages = get_packages();

        packages.retain(|package| -> bool {
            for accept in package.accepts.iter() {
                if (accept.starts_with('*') && path.ends_with(&accept[1..]))
                    || (accept.ends_with('*') && path.starts_with(&accept[..accept.len() - 1]))
                {
                    return true;
                }
            }
            false
        });

        if packages.len() > 1 {
            let mut window = Window::new(
                -1,
                -1,
                chooser_width(),
                packages.len() as u32 * icon_small_size() as u32,
                path,
            )
            .expect("launcher: failed to open window");
            let font = Font::find(Some("Sans"), None, None).expect("launcher: failed to open font");

            let mut selected = -1;
            let mut mouse_y = 0;
            let mut mouse_left = false;
            let mut last_mouse_left = false;

            draw_chooser(&mut window, &font, &mut packages, selected, None);
            'choosing: loop {
                for event in window.events() {
                    let redraw = match event.to_option() {
                        EventOption::Mouse(mouse_event) => {
                            mouse_y = mouse_event.y;
                            true
                        }
                        EventOption::Button(button_event) => {
                            mouse_left = button_event.left;
                            true
                        }
                        EventOption::Quit(_) => break 'choosing,
                        _ => false,
                    };

                    if redraw {
                        let mut now_selected = -1;

                        let mut y = 0;
                        for (i, _package) in packages.iter().enumerate() {
                            if mouse_y >= y && mouse_y < y + icon_size() {
                                now_selected = i as i32;
                            }
                            y += icon_small_size();
                        }

                        if now_selected != selected {
                            selected = now_selected;
                            draw_chooser(&mut window, &font, &mut packages, selected, None);
                        }

                        if !mouse_left && last_mouse_left {
                            let mut y = 0;
                            for package in packages.iter() {
                                if mouse_y >= y && mouse_y < y + icon_small_size() {
                                    spawn_exec(&package.exec, Some(&path));
                                    break 'choosing;
                                }
                                y += icon_small_size();
                            }
                        }

                        last_mouse_left = mouse_left;
                    }
                }
            }
        } else if let Some(package) = packages.get(0) {
            spawn_exec(&package.exec, Some(&path));
        } else {
            error!("no application found for '{}'", path);
        }
    }
}

fn start_logging() {
    if let Err(e) = RedoxLogger::new()
        .with_output(
            OutputBuilder::stdout()
                .with_filter(log::LevelFilter::Warn)
                .with_ansi_escape_codes()
                .build(),
        )
        .with_process_name("launcher".into())
        .enable()
    {
        eprintln!("Launcher could not start logging: {}", e);
    }
}

fn main() -> Result<(), String> {
    start_logging();

    let (width, height) = orbclient::get_display_size()?;
    SCALE.store((height as isize / 1600) + 1, Ordering::Relaxed);
    let paths = env::args();
    if paths.len() > 1 {
        chooser_main(paths);
    } else {
        bar_main(width, height).map_err(|e| e.to_string())?;
    }

    Ok(())
}
