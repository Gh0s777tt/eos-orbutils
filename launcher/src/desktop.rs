//! E-OS desktop icons: labelled application and ~/Desktop file icons on a
//! small transparent Back window. The bar waits for our DESKTOP-READY line
//! before it spawns `background` — orbital keeps creation order among Back
//! windows (earlier = closer to the viewer), so the icons always sit above
//! the animated wallpaper.

#[allow(dead_code)]
mod package;
#[allow(dead_code)]
mod theme;
#[allow(dead_code)]
mod ui;

// package.rs resolves icon helpers through the crate root, same as in main.rs
pub use ui::*;

use std::env;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use log::{error, warn};
use orbclient::{Color, EventOption, Mode, Renderer, Window, WindowFlag};
use orbfont::Font;
use redox_log::{OutputBuilder, RedoxLogger};

use package::{Icon, IconSource};
use theme::TEXT_COLOR;

const HIGHLIGHT_FILL: Color = Color::rgba(229, 9, 20, 46);
const HIGHLIGHT_BORDER: Color = Color::rgba(229, 9, 20, 140);
const SHADOW_COLOR: Color = Color::rgba(0, 0, 0, 200);
const PLACEHOLDER_FILL: Color = Color::rgba(96, 10, 16, 255);
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

enum Action {
    Exec(String),
    Open(String),
}

struct Entry {
    name: String,
    icon: Icon,
    action: Action,
}

fn cell_pad() -> i32 {
    8 * SCALE.load(Ordering::Relaxed) as i32
}

fn cell_width() -> i32 {
    icon_size() * 2
}

fn cell_height() -> i32 {
    icon_size() + font_size() + 3 * cell_pad()
}

fn desktop_entries() -> Vec<Entry> {
    let mut entries = Vec::new();

    // The same app often shows up twice: once as a UI_PATH manifest and once
    // as an XDG desktop entry. Key by the exec binary name and prefer the XDG
    // variant (localized name, themed icon).
    let mut seen = std::collections::BTreeMap::<String, (usize, bool)>::new();
    for package in get_packages() {
        if package.exec.is_empty() {
            continue;
        }
        let key = package
            .exec
            .split_whitespace()
            .next()
            .map(|tok| tok.rsplit('/').next().unwrap_or(tok).to_string())
            .unwrap_or_else(|| package.name.clone());
        let xdg = package.id.ends_with(".desktop");
        let entry = Entry {
            name: package.name.clone(),
            icon: package.icon.clone(),
            action: Action::Exec(package.exec.clone()),
        };
        match seen.get(&key).copied() {
            Some((index, was_xdg)) => {
                if xdg && !was_xdg {
                    entries[index] = entry;
                    seen.insert(key, (index, true));
                }
            }
            None => {
                seen.insert(key, (entries.len(), xdg));
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    if let Ok(home) = env::var("HOME") {
        if let Ok(read_dir) = Path::new(&home).join("Desktop").read_dir() {
            let mut files: Vec<_> = read_dir.flatten().collect();
            files.sort_by_key(|entry| entry.file_name());
            for file in files {
                let Ok(file_type) = file.file_type() else {
                    continue;
                };
                if !file_type.is_file() {
                    continue;
                }
                let name = file.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let mut icon = Icon::empty(false);
                icon.source = IconSource::Name("text-x-generic".to_string());
                entries.push(Entry {
                    name,
                    icon,
                    action: Action::Open(file.path().display().to_string()),
                });
            }
        }
    }

    entries
}

fn activate(entry: &Entry) {
    match &entry.action {
        Action::Exec(exec) => spawn_exec(exec, None),
        Action::Open(path) => match Command::new("launcher").arg(path).spawn() {
            Ok(_) => {}
            Err(err) => error!("failed to open {}: {}", path, err),
        },
    }
}

fn truncate_to_width(font: &Font, name: &str, max_width: i32) -> String {
    let text_width = |s: &str| font.render(s, font_size() as f32).width() as i32;
    if text_width(name) <= max_width {
        return name.to_string();
    }
    let mut out = String::new();
    for c in name.chars() {
        if text_width(&format!("{}{}…", out, c)) > max_width {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}

fn cell_at(x: i32, y: i32, rows: usize, len: usize) -> i32 {
    if x < 0 || y < 0 {
        return -1;
    }
    let col = x / cell_width();
    let row = y / cell_height();
    if (row as usize) >= rows {
        return -1;
    }
    let i = col as usize * rows + row as usize;
    if i < len {
        i as i32
    } else {
        -1
    }
}

fn draw(window: &mut Window, font: &Font, entries: &mut [Entry], rows: usize, selected: i32) {
    window.set(Color::rgba(0, 0, 0, 0));

    for (i, entry) in entries.iter_mut().enumerate() {
        let col = (i / rows) as i32;
        let row = (i % rows) as i32;
        let x0 = col * cell_width();
        let y0 = row * cell_height();
        let cw = cell_width();
        let ch = cell_height();

        if i as i32 == selected {
            // Overwrite: blending semi-transparent color onto the alpha-0 base
            // premultiplies RGB client-side and orbital multiplies by alpha
            // again at composite — the highlight would all but disappear.
            window.mode().set(Mode::Overwrite);
            window.rect(x0 + 2, y0 + 2, (cw - 4) as u32, (ch - 4) as u32, HIGHLIGHT_FILL);
            window.rect(x0 + 2, y0 + 2, (cw - 4) as u32, 1, HIGHLIGHT_BORDER);
            window.rect(x0 + 2, y0 + ch - 3, (cw - 4) as u32, 1, HIGHLIGHT_BORDER);
            window.rect(x0 + 2, y0 + 2, 1, (ch - 4) as u32, HIGHLIGHT_BORDER);
            window.rect(x0 + cw - 3, y0 + 2, 1, (ch - 4) as u32, HIGHLIGHT_BORDER);
            window.mode().set(Mode::Blend);
        }

        let iy = y0 + cell_pad();
        let image = entry.icon.image();
        if image.width() > 0 {
            let ix = x0 + (cw - image.width() as i32) / 2;
            window.image(ix, iy, image.width(), image.height(), image.data());
        } else {
            // placeholder tile with the entry's initial
            let px = x0 + (cw - icon_size()) / 2;
            window.rect(px, iy, icon_size() as u32, icon_size() as u32, PLACEHOLDER_FILL);
            if let Some(letter) = entry.name.chars().next() {
                let glyph: String = letter.to_uppercase().collect();
                let text = font.render(&glyph, (icon_size() / 2) as f32);
                text.draw(
                    window,
                    px + (icon_size() - text.width() as i32) / 2,
                    iy + icon_size() / 4,
                    TEXT_COLOR,
                );
            }
        }

        let label = truncate_to_width(font, &entry.name, cw - cell_pad());
        let text = font.render(&label, font_size() as f32);
        let tx = x0 + (cw - text.width() as i32) / 2;
        let ty = y0 + cell_pad() + icon_size() + cell_pad() / 2;
        // 1px shadow keeps captions readable over bright smoke
        text.draw(window, tx + 1, ty + 1, SHADOW_COLOR);
        text.draw(window, tx, ty, TEXT_COLOR);
    }

    window.sync();
}

fn main() {
    let _ = RedoxLogger::new()
        .with_output(
            OutputBuilder::stdout()
                .with_filter(log::LevelFilter::Warn)
                .with_ansi_escape_codes()
                .build(),
        )
        .with_process_name("desktop".into())
        .enable();

    let (display_w, display_h) =
        orbclient::get_display_size().expect("desktop: failed to get display size");
    SCALE.store((display_h as isize / 1600) + 1, Ordering::Relaxed);

    let mut entries = desktop_entries();

    let usable_h = ((display_h as i32) * 7) / 10;
    let max_rows = ((usable_h - 2 * cell_pad()) / cell_height()).max(1) as usize;
    let rows = max_rows.min(entries.len().max(1));
    let max_cols = (((display_w as i32) - 4 * cell_pad()) / cell_width()).max(1) as usize;
    let cols = entries.len().div_ceil(rows).max(1).min(max_cols);
    if entries.len() > rows * cols {
        warn!(
            "desktop: {} entries do not fit on screen, showing the first {}",
            entries.len(),
            rows * cols
        );
        entries.truncate(rows * cols);
    }

    let mut window = Window::new_flags(
        2 * cell_pad(),
        2 * cell_pad(),
        (cols as i32 * cell_width()) as u32,
        (rows as i32 * cell_height()) as u32,
        "",
        &[
            WindowFlag::Back,
            WindowFlag::Borderless,
            WindowFlag::Transparent,
            WindowFlag::Unclosable,
        ],
    )
    .expect("desktop: failed to open window");

    // The bar blocks on this line before spawning `background`, so the
    // wallpaper window is guaranteed to be created after ours.
    println!("DESKTOP-READY");

    let font = Font::find(Some("Sans"), None, None).expect("desktop: failed to open font");

    let mut selected: i32 = -1;
    draw(&mut window, &font, &mut entries, rows, selected);

    let mut mouse_x = 0;
    let mut mouse_y = 0;
    let mut mouse_left = false;
    let mut last_mouse_left = false;
    let mut last_click: Option<(i32, Instant)> = None;

    'events: loop {
        for event in window.events() {
            match event.to_option() {
                EventOption::Mouse(mouse_event) => {
                    mouse_x = mouse_event.x;
                    mouse_y = mouse_event.y;
                }
                EventOption::Button(button_event) => {
                    mouse_left = button_event.left;
                }
                EventOption::Quit(_) => break 'events,
                _ => continue,
            }

            if !mouse_left && last_mouse_left {
                let cell = cell_at(mouse_x, mouse_y, rows, entries.len());
                if cell >= 0 {
                    let double = matches!(
                        last_click,
                        Some((c, t)) if c == cell && t.elapsed() < DOUBLE_CLICK
                    );
                    if double {
                        activate(&entries[cell as usize]);
                        last_click = None;
                        reap();
                    } else {
                        last_click = Some((cell, Instant::now()));
                    }
                }
                if cell != selected {
                    selected = cell;
                    draw(&mut window, &font, &mut entries, rows, selected);
                }
            }
            last_mouse_left = mouse_left;
        }

        reap();
    }
}

/// Collect exited children (double-click-launched apps) so they do not linger
/// as zombies. Best effort: runs after activations and window-event batches.
fn reap() {
    let mut status = 0;
    while ui::wait(&mut status).unwrap_or(0) > 0 {}
}
