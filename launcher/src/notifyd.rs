//! `eos-notifyd` — a minimal desktop-notification daemon.
//!
//! It polls `/tmp/eos-notify` for a `"title\nbody"` message (written by
//! `eos-notify`) and shows it as a crimson toast in the top-right corner for a
//! few seconds, then removes the file so it fires once. Started by the launcher
//! alongside `desktop`/`background`.
//!
//! Deliberately minimal: the file transport is a placeholder for a proper
//! `notify:` scheme / Unix socket, and a toast blocks new ones while it's up.
//! Enough for the update daemon to surface "updates available" (R-D03).

extern crate orbclient;
extern crate orbfont;

use orbclient::{Color, Renderer, Window, WindowFlag};
use orbfont::Font;
use std::{fs, thread, time::Duration};

const NOTIFY_PATH: &str = "/tmp/eos-notify";
const TOAST_W: u32 = 340;
const TOAST_H: u32 = 84;
const SHOW_SECS: u64 = 4;

// E-OS Crimson palette (kept in sync with the launcher's theme).
const PANEL: Color = Color::rgba(22, 3, 3, 235);
const TEXT: Color = Color::rgb(0xE7, 0xE7, 0xE7);
const ACCENT: Color = Color::rgb(0xE5, 0x09, 0x14);

fn main() {
    let font = Font::find(Some("Sans"), None, None).ok();
    loop {
        let content = fs::read_to_string(NOTIFY_PATH).unwrap_or_default();
        if !content.trim().is_empty() {
            // Consume it so the toast fires exactly once.
            let _ = fs::remove_file(NOTIFY_PATH);
            let mut parts = content.splitn(2, '\n');
            let title = parts.next().unwrap_or("").trim().to_string();
            let body = parts.next().unwrap_or("").trim().to_string();
            show_toast(font.as_ref(), &title, &body);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Draw one toast top-right and keep it up for `SHOW_SECS`, then let the window
/// drop (which closes it).
fn show_toast(font: Option<&Font>, title: &str, body: &str) {
    let (screen_w, _) = orbclient::get_display_size().unwrap_or((800, 600));
    let x = screen_w as i32 - TOAST_W as i32 - 16;
    let Some(mut win) = Window::new_flags(
        x,
        16,
        TOAST_W,
        TOAST_H,
        "notification",
        &[
            WindowFlag::Borderless,
            WindowFlag::Transparent,
            WindowFlag::Front,
        ],
    ) else {
        return;
    };

    win.set(Color::rgba(0, 0, 0, 0));
    win.rounded_rect(0, 0, TOAST_W, TOAST_H, 10, true, PANEL);
    win.rect(0, 0, 4, TOAST_H, ACCENT); // crimson accent bar
    if let Some(font) = font {
        font.render(title, 18.0).draw(&mut win, 18, 14, ACCENT);
        if !body.is_empty() {
            font.render(body, 14.0).draw(&mut win, 18, 46, TEXT);
        }
    }
    win.sync();

    thread::sleep(Duration::from_secs(SHOW_SECS));
}
