//! E-OS Settings — a native "Crimson" control panel built directly on
//! orbital/orbclient, with NO libcosmic / fontconfig dependency (so it compiles
//! on the aarch64 dev host and sidesteps the cosmic-settings host-toolchain gap
//! and the dead CI). Foundation B (`R-D01`): a panel host that will later carry
//! Settings -> Update (`R-708`) and Settings -> Drivers (`R-806`).

#[allow(dead_code)]
mod package;
#[allow(dead_code)]
mod theme;
#[allow(dead_code)]
mod ui;

// package.rs resolves helpers through the crate root, same as the other bins.
pub use ui::*;

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use orbclient::{Color, EventOption, Renderer, Window, K_ESC};
use orbfont::Font;

// Crimson palette (#E50914 on near-black), consistent with theme.rs.
const BG: Color = Color::rgb(12, 8, 8);
const SIDEBAR_BG: Color = Color::rgb(20, 12, 12);
const CRIMSON: Color = Color::rgb(0xE5, 0x09, 0x14);
const ACTIVE_BG: Color = Color::rgba(0xE5, 0x09, 0x14, 40);
const TEXT: Color = Color::rgb(0xEC, 0xE7, 0xE5);
const MUTED: Color = Color::rgb(0x9A, 0x90, 0x8C);
const DIM: Color = Color::rgb(0x6F, 0x66, 0x63);
const LINE: Color = Color::rgb(42, 30, 28);
const GOOD: Color = Color::rgb(0x37, 0xB9, 0x6B);

const WIN_W: u32 = 780;
const WIN_H: u32 = 520;
const SIDEBAR_W: i32 = 210;
const HEADER_H: i32 = 64;
const ROW_H: i32 = 40;

const PANELS: &[&str] = &[
    "System",
    "Bezpieczeństwo",
    "Aktualizacje",
    "Sterowniki",
    "Sieć",
    "Ekran",
    "Dźwięk",
    "Data i czas",
    "Użytkownik",
];

/// One rendered content line: a labelled key/value, or a full-width note.
struct Row {
    label: String,
    value: String,
    good: bool,
}
impl Row {
    fn kv(label: &str, value: &str) -> Row {
        Row { label: label.into(), value: value.into(), good: false }
    }
    fn ok(label: &str, value: &str) -> Row {
        Row { label: label.into(), value: value.into(), good: true }
    }
    fn note(value: &str) -> Row {
        Row { label: String::new(), value: value.into(), good: false }
    }
}

fn read_trim(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Pull KEY=value from the E-OS/os release files.
fn os_release(key: &str) -> Option<String> {
    for path in ["/usr/share/eos/eos-release", "/etc/os-release"] {
        if let Ok(s) = fs::read_to_string(path) {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix(key) {
                    if let Some(v) = rest.strip_prefix('=') {
                        return Some(v.trim().trim_matches('"').to_string());
                    }
                }
            }
        }
    }
    None
}

fn panel_rows(idx: usize) -> Vec<Row> {
    match idx {
        0 => vec![
            Row::kv(
                "System",
                &os_release("PRETTY_NAME")
                    .or_else(|| os_release("NAME"))
                    .unwrap_or_else(|| "E-OS Crimson".into()),
            ),
            Row::kv(
                "Wersja",
                &os_release("VERSION").unwrap_or_else(|| "0.1.0 \"Genesis\"".into()),
            ),
            Row::kv("Architektura", std::env::consts::ARCH),
            Row::kv("Host", &read_trim("/etc/hostname").unwrap_or_else(|| "eos".into())),
            Row::kv("Jądro", "Redox (mikrojądro, fork E-OS)"),
        ],
        1 => vec![
            Row::ok("Szyfrowanie dysku (FDE)", "AES-XTS · RedoxFS"),
            Row::ok("W⊕X", "wymuszone na granicy syscalli"),
            Row::ok("ASLR", "mmap + ld.so"),
            Row::ok("overflow-checks", "kernel + base + relibc"),
            Row::ok(
                "RAID-1 mirror",
                if Path::new("/scheme/disk.raid1").exists() {
                    "aktywny (raid1d)"
                } else {
                    "dostępny (raid1d)"
                },
            ),
            Row::ok("Podpisy pakietów", "ed25519 + ML-DSA-65 (hybrydowe PQ)"),
            Row::note("Pulpit bez telemetrii — dane pozostają lokalnie."),
        ],
        2 => {
            let mut rows = vec![Row::note(
                "System aktualizacji (Settings → Update) — w budowie (R-7xx).",
            )];
            if let Ok(read_dir) = Path::new("/etc/pkg.d").read_dir() {
                for entry in read_dir.flatten() {
                    rows.push(Row::kv("Źródło", &entry.file_name().to_string_lossy()));
                }
            }
            rows
        }
        3 => vec![
            Row::note("Menedżer sterowników (Settings → Drivers) — w budowie (R-8xx)."),
            Row::note("Wykrywanie sprzętu + instalacja wyłącznie z podpisanego repo E-OS."),
        ],
        4 => vec![
            Row::kv("Host", &read_trim("/etc/hostname").unwrap_or_else(|| "eos".into())),
            Row::note("Panel sieci — w budowie (R-902). Dziś: schematy netcfg / ifconfig."),
        ],
        5 => vec![Row::note("Ustawienia ekranu — w budowie. Kompozytor: orbital.")],
        6 => vec![Row::note("Miks i głośność — w budowie (R-D07). Sterowniki: ihdad / ac97d.")],
        7 => {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
            vec![
                Row::kv("Czas (UTC)", &format!("{:02}:{:02}:{:02}", h, m, s)),
                Row::note("Strefa czasowa: UTC. Lokalny czas + data — R-D05."),
            ]
        }
        8 => vec![
            Row::kv(
                "Użytkownik",
                &std::env::var("USER").unwrap_or_else(|_| "user".into()),
            ),
            Row::note("Zarządzanie kontami — w budowie. Kreator first-boot: R-602."),
        ],
        _ => Vec::new(),
    }
}

fn render(window: &mut Window, font: &Font, active: usize) {
    let w = window.width() as i32;
    let h = window.height() as i32;

    window.set(BG);

    // Sidebar.
    window.rect(0, 0, SIDEBAR_W as u32, h as u32, SIDEBAR_BG);
    window.rect(SIDEBAR_W, 0, 1, h as u32, LINE);

    // Wordmark: crimson "E" + "-OS Settings".
    font.render("E", 28.0).draw(window, 20, 16, CRIMSON);
    font.render("-OS  Settings", 16.0).draw(window, 48, 26, TEXT);

    // Panel list.
    for (i, name) in PANELS.iter().enumerate() {
        let y = HEADER_H + i as i32 * ROW_H;
        if i == active {
            window.rect(0, y, SIDEBAR_W as u32, ROW_H as u32, ACTIVE_BG);
            window.rect(0, y, 3, ROW_H as u32, CRIMSON);
        }
        let color = if i == active { TEXT } else { MUTED };
        font.render(name, 15.0).draw(window, 24, y + 11, color);
    }

    // Content header.
    let cx = SIDEBAR_W + 28;
    font.render(PANELS[active], 24.0).draw(window, cx, 22, TEXT);
    window.rect(cx, 58, (w - cx - 28).max(0) as u32, 1, LINE);

    // Content rows.
    let mut y = 82;
    for row in panel_rows(active) {
        if row.label.is_empty() {
            font.render(&row.value, 13.0).draw(window, cx, y, MUTED);
        } else {
            font.render(&row.label, 14.0).draw(window, cx, y, MUTED);
            let value_color = if row.good { GOOD } else { TEXT };
            font.render(&row.value, 14.0).draw(window, cx + 210, y, value_color);
        }
        y += 30;
    }

    // Footer.
    font.render(
        "E-OS Crimson · natywny panel bez libcosmic · R-D01",
        11.0,
    )
    .draw(window, cx, h - 26, DIM);

    window.sync();
}

fn main() {
    let mut window = Window::new(120, 80, WIN_W, WIN_H, "E-OS Settings")
        .expect("settings: failed to open window");
    let font = Font::find(Some("Sans"), None, None).expect("settings: failed to open font");

    let mut active: usize = 0;
    render(&mut window, &font, active);

    let mut mouse_x = 0;
    let mut mouse_y = 0;
    let mut mouse_left = false;
    let mut last_mouse_left = false;

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
                EventOption::Key(key_event) => {
                    if key_event.pressed && key_event.scancode == K_ESC {
                        break 'events;
                    }
                }
                EventOption::Quit(_) => break 'events,
                _ => continue,
            }

            // Click released inside the sidebar -> switch panel.
            if !mouse_left && last_mouse_left && mouse_x < SIDEBAR_W && mouse_y >= HEADER_H {
                let idx = ((mouse_y - HEADER_H) / ROW_H) as usize;
                if idx < PANELS.len() && idx != active {
                    active = idx;
                    render(&mut window, &font, active);
                }
            }
            last_mouse_left = mouse_left;
        }
    }
}
