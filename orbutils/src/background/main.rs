use image::RgbaImage;
use libredox::flag;
use log::{error, warn};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    os::unix::io::{AsRawFd, FromRawFd, RawFd},
    path::PathBuf,
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime},
};
use xxhash_rust::const_xxh3::xxh3_64;

use orbclient::image::{Image, ImageError};
use orbclient::{Color, EventOption, Renderer, Window, WindowFlag};
use redox_log::{OutputBuilder, RedoxLogger};

struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
enum BackgroundMode {
    /// Do not resize the image, just center it
    Center,
    /// Resize the image to the display size
    Fill,
    /// Resize the image - keeping its aspect ratio, and fit it to the display with blank space
    Scale,
    /// Resize the image - keeping its aspect ratio, and crop to remove all blank space
    Zoom,
}

impl BackgroundMode {
    fn from_str(string: &str) -> BackgroundMode {
        match string {
            "center" => BackgroundMode::Center,
            "fill" => BackgroundMode::Fill,
            "scale" => BackgroundMode::Scale,
            _ => BackgroundMode::Zoom,
        }
    }
}

fn find_scale(
    image: &Image,
    mode: BackgroundMode,
    display_width: u32,
    display_height: u32,
) -> (u32, u32) {
    match mode {
        BackgroundMode::Center => (image.width(), image.height()),
        BackgroundMode::Fill => (display_width, display_height),
        BackgroundMode::Scale => {
            let d_w = display_width as f64;
            let d_h = display_height as f64;
            let i_w = image.width() as f64;
            let i_h = image.height() as f64;

            let scale = if d_w / d_h > i_w / i_h {
                d_h / i_h
            } else {
                d_w / i_w
            };

            ((i_w * scale) as u32, (i_h * scale) as u32)
        }
        BackgroundMode::Zoom => {
            let d_w = display_width as f64;
            let d_h = display_height as f64;
            let i_w = image.width() as f64;
            let i_h = image.height() as f64;

            let scale = if d_w / d_h < i_w / i_h {
                d_h / i_h
            } else {
                d_w / i_w
            };

            ((i_w * scale) as u32, (i_h * scale) as u32)
        }
    }
}

fn find_background() -> String {
    match dirs::home_dir() {
        Some(home) => {
            for name in &["background.png", "background.jpg"] {
                let path = home.join(name);
                if path.is_file() {
                    if let Some(path_str) = path.to_str() {
                        return path_str.to_string();
                    }
                }
            }
        }
        _ => (),
    }

    "/usr/share/ui/background.jpg".to_string()
}

/// returns the cache path and cache hash
fn get_cached_background(path: &str, mode: BackgroundMode, w: u32, h: u32) -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?.join("backgrounds");

    if !cache_dir.is_dir() {
        if let Err(e) = fs::create_dir_all(&cache_dir) {
            warn!("Unable to create cache directory: {e:?}");
            return None;
        }
    }

    let mtime = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let input = format!("{}:{}:{:?}:{}x{}", path, mtime, mode, w, h);
    let hash = xxh3_64(input.as_bytes());

    Some(cache_dir.join(format!("{:x}.bmp", hash)))
}

static CACHED_IMAGES: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn scale_and_cache(
    source: &str,
    mode: BackgroundMode,
    w: u32,
    h: u32,
    should_cache: bool,
) -> Result<Image, ImageError> {
    let cache_path = get_cached_background(source, mode, w, h);

    if let Some(ref path) = cache_path {
        if path.exists() {
            if let Ok(mut cached_images) = CACHED_IMAGES.lock() {
                if let Some(name) = path.file_name().map(|x| x.to_string_lossy()) {
                    cached_images.insert(name.to_string());
                }
            }
            if let Ok(img) = Image::from_path(path) {
                return Ok(img);
            }
        }
    }

    let original = Image::from_path(source)?;

    let (width, height) = find_scale(&original, mode, w, h);

    let scaled = if width == original.width() && height == original.height() {
        original
    } else {
        original.resize(width, height, orbclient::image::ResizeType::Lanczos3)
    };

    if should_cache {
        if let Some(path) = cache_path {
            let width = scaled.width();
            let height = scaled.height();
            let data = scaled.data();

            let mut rgba_bytes = Vec::with_capacity((width * height * 4) as usize);
            for color in data.iter() {
                rgba_bytes.extend_from_slice(&[color.r(), color.g(), color.b(), color.a()]);
            }

            if let Some(img_buffer) = RgbaImage::from_raw(width, height, rgba_bytes) {
                if let Err(err) = img_buffer.save(&path) {
                    warn!("Unable to write background cache: {err:?}");
                } else {
                    if let Ok(mut cached_images) = CACHED_IMAGES.lock() {
                        if let Some(name) = path.file_name().map(|x| x.to_string_lossy()) {
                            cached_images.insert(name.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(scaled)
}

fn remove_unused_cache() -> std::io::Result<()> {
    let Some(cache_dir) = dirs::cache_dir().map(|x| x.join("backgrounds")) else {
        return Ok(());
    };

    if !cache_dir.is_dir() {
        return Ok(());
    }

    let paths = fs::read_dir(&cache_dir)?;

    let Ok(cached_images) = CACHED_IMAGES.lock() else {
        return Ok(());
    };

    for path in paths {
        let Ok(path) = path else {
            continue;
        };
        let name = path.file_name().to_string_lossy().to_string();
        if !cached_images.contains(&name) {
            fs::remove_file(path.path())?;
        }
    }

    Ok(())
}

fn get_full_url(path: &str) -> Result<String, String> {
    let file = match libredox::call::open(path, flag::O_CLOEXEC | flag::O_PATH, 0) {
        Ok(ok) => unsafe { File::from_raw_fd(ok as RawFd) },
        Err(err) => return Err(format!("{}", err)),
    };

    let mut buf: [u8; 4096] = [0; 4096];
    let count = libredox::call::fpath(file.as_raw_fd() as usize, &mut buf)
        .map_err(|err| format!("{}", err))?;

    String::from_utf8(Vec::from(&buf[..count])).map_err(|err| format!("{}", err))
}

//TODO: determine x, y of display by talking to orbital instead of guessing!
fn get_display_rects() -> Result<Vec<DisplayRect>, String> {
    let url = get_full_url(&env::var("DISPLAY").or(Err("DISPLAY not set"))?)?;

    let mut url_parts = url.split(':');
    let scheme_name = url_parts.next().ok_or(format!("no scheme name"))?;
    let path = url_parts.next().ok_or(format!("no path"))?;

    let mut path_parts = path.split('/');
    let vt_screen = path_parts.next().unwrap_or("");
    let width = path_parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);
    let height = path_parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);

    let mut display_rects = vec![DisplayRect {
        x: 0,
        y: 0,
        width,
        height,
    }];

    // If display server supports multiple displays in a VT
    if vt_screen.contains('.') {
        // Look for other screens in the same VT
        let mut parts = vt_screen.split('.');
        let vt_i = parts.next().unwrap_or("").parse::<usize>().unwrap_or(0);
        let start_screen_i = parts.next().unwrap_or("").parse::<usize>().unwrap_or(0);
        //TODO: determine maximum number of screens
        for screen_i in start_screen_i + 1..1024 {
            let url = match get_full_url(&format!("/scheme/{}/{}.{}", scheme_name, vt_i, screen_i))
            {
                Ok(ok) => ok,
                //TODO: only check for ENOENT?
                Err(_err) => break,
            };

            let mut url_parts = url.split(':');
            let _scheme_name = url_parts.next().ok_or(format!("no scheme name"))?;
            let path = url_parts.next().ok_or(format!("no path"))?;

            let mut path_parts = path.split('/');
            let _vt_screen = path_parts.next().unwrap_or("");
            let width = path_parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);
            let height = path_parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);

            let x = if let Some(last) = display_rects.last() {
                last.x + last.width as i32
            } else {
                0
            };

            display_rects.push(DisplayRect {
                x,
                y: 0,
                width,
                height,
            });
        }
    }

    Ok(display_rects)
}

// ===== E-OS Crimson: animated smoke & sparks over the wallpaper =====
//
// Software rendering budget: sprites are pre-rendered per (color, size, alpha
// level). Slow smoke advances at half the frame rate (15 Hz), fast sparks at
// full rate; each frame only dirty 32px tiles are restored from the static
// base image, only particles touching a dirty tile are re-blended, and the
// damage sent to orbital is the merged tile spans. Steady-state cost stays
// bounded by the smoke band (lower ~60% of the screen at 15 Hz), never the
// full window per frame.

const FRAME_MS: u64 = 33; // ~30 fps
const FRAME_DT: f32 = 0.033;
const SMOKE_DT: f32 = 0.0667; // smoke advances every second frame
const TILE: i32 = 32;
const ALPHA_LEVELS: usize = 12;

const SMOKE_COLORS: [(u8, u8, u8, f32); 3] = [
    (229, 9, 20, 80.0), // brand crimson
    (160, 14, 22, 72.0),
    (96, 10, 16, 64.0),
];
const SMOKE_SIZES: [u32; 5] = [16, 22, 30, 40, 48];
const SPARK_COLORS: [(u8, u8, u8, f32); 2] = [(255, 184, 92, 240.0), (255, 96, 32, 230.0)];
const SPARK_SIZES: [u32; 3] = [3, 5, 7];

struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }

    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// uniform in [0, 1)
    fn f(&mut self) -> f32 {
        (self.next() >> 8) as f32 / 16_777_216.0
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f()
    }
}

struct Sprite {
    w: i32,
    h: i32,
    data: Vec<Color>,
}

/// A soft radial puff: alpha falls off quadratically to zero at the edge, so
/// the sprite is "pre-blurred" and needs no runtime filtering.
fn make_puff(size: u32, r: u8, g: u8, b: u8, peak: f32) -> Sprite {
    let w = size as i32;
    let h = w;
    let c = (w as f32 - 1.0) / 2.0;
    let radius = w as f32 / 2.0;
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            let t = (1.0 - d / radius).max(0.0);
            let a = (peak * t * t).round().min(255.0) as u8;
            data.push(Color::rgba(r, g, b, a));
        }
    }
    Sprite { w, h, data }
}

/// sprites[color][size][alpha level]; level i is scaled to (i+1)/ALPHA_LEVELS
/// of the peak alpha, so per-particle fades cost a single index change.
struct SpriteBank {
    smoke: Vec<Vec<Vec<Sprite>>>,
    spark: Vec<Vec<Vec<Sprite>>>,
}

impl SpriteBank {
    fn new() -> Self {
        let build = |colors: &[(u8, u8, u8, f32)], sizes: &[u32]| -> Vec<Vec<Vec<Sprite>>> {
            colors
                .iter()
                .map(|&(r, g, b, peak)| {
                    sizes
                        .iter()
                        .map(|&s| {
                            (0..ALPHA_LEVELS)
                                .map(|lvl| {
                                    let scale = (lvl + 1) as f32 / ALPHA_LEVELS as f32;
                                    make_puff(s, r, g, b, peak * scale)
                                })
                                .collect()
                        })
                        .collect()
                })
                .collect()
        };
        SpriteBank {
            smoke: build(&SMOKE_COLORS, &SMOKE_SIZES),
            spark: build(&SPARK_COLORS, &SPARK_SIZES),
        }
    }
}

struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: f32,
    life: f32,
    seed: f32,
    color_i: usize,
    size_i: usize,
    spark: bool,
    /// box drawn last frame (x, y, w, h) — the region to restore
    prev_box: Option<(i32, i32, i32, i32)>,
}

/// Fade envelope over lifetime and height; 0 means invisible this frame,
/// otherwise use sprite alpha level (result - 1).
fn alpha_level(p: &Particle, display_h: f32) -> usize {
    let t = p.age / p.life;
    let env = (t / 0.15).min((1.0 - t) / 0.35).clamp(0.0, 1.0);
    let mut a = if p.spark {
        // sparks flicker instead of fading by height
        env * (0.7 + 0.3 * (p.age * 26.0 + p.seed).sin())
    } else {
        // smoke dissolves as it climbs past ~40% of the screen height
        env * ((p.y - 0.40 * display_h) / (0.18 * display_h)).clamp(0.0, 1.0)
    };
    a = a.clamp(0.0, 1.0);
    (a * ALPHA_LEVELS as f32) as usize
}

fn sprite_of<'a>(bank: &'a SpriteBank, p: &Particle, display_h: f32) -> Option<&'a Sprite> {
    let lvl = alpha_level(p, display_h);
    if lvl == 0 {
        return None;
    }
    if p.spark {
        let size_i = p.size_i.min(SPARK_SIZES.len() - 1);
        Some(&bank.spark[p.color_i][size_i][lvl - 1])
    } else {
        // smoke expands as it ages
        let grow = (p.age / p.life * 2.5) as usize;
        let size_i = (p.size_i + grow).min(SMOKE_SIZES.len() - 1);
        Some(&bank.smoke[p.color_i][size_i][lvl - 1])
    }
}

/// Alpha-blend a sprite into the window buffer with full edge clipping
/// (orbclient's image_fast drops sprites with negative coordinates).
fn blend_sprite(dst: &mut [Color], dst_w: i32, dst_h: i32, spr: &Sprite, px: i32, py: i32) {
    let x0 = px.max(0);
    let y0 = py.max(0);
    let x1 = (px + spr.w).min(dst_w);
    let y1 = (py + spr.h).min(dst_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for y in y0..y1 {
        let src_row = ((y - py) * spr.w) as usize;
        let dst_row = (y * dst_w) as usize;
        for x in x0..x1 {
            let s = spr.data[src_row + (x - px) as usize].data;
            let a = (s >> 24) & 0xFF;
            if a == 0 {
                continue;
            }
            let d = &mut dst[dst_row + x as usize].data;
            if a >= 255 {
                *d = s;
            } else {
                let n_alpha = 255 - a;
                let rb = ((n_alpha * (*d & 0x00FF00FF)) + (a * (s & 0x00FF00FF))) >> 8;
                let ag =
                    (n_alpha * ((*d & 0xFF00FF00) >> 8)) + (a * (0x01000000 | ((s & 0x0000FF00) >> 8)));
                *d = (rb & 0x00FF00FF) | (ag & 0xFF00FF00);
            }
        }
    }
}

fn mark_tiles(dirty: &mut [bool], tiles_x: i32, w: i32, h: i32, rect: (i32, i32, i32, i32)) {
    let (x, y, rw, rh) = rect;
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + rw).min(w);
    let y1 = (y + rh).min(h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for ty in (y0 / TILE)..=((y1 - 1) / TILE) {
        for tx in (x0 / TILE)..=((x1 - 1) / TILE) {
            dirty[(ty * tiles_x + tx) as usize] = true;
        }
    }
}

/// Does the particle box overlap any dirty tile? Unchanged particles are only
/// re-blended when something else disturbed the tiles under them.
fn hits_dirty(dirty: &[bool], tiles_x: i32, w: i32, h: i32, rect: (i32, i32, i32, i32)) -> bool {
    let (x, y, rw, rh) = rect;
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + rw).min(w);
    let y1 = (y + rh).min(h);
    if x0 >= x1 || y0 >= y1 {
        return false;
    }
    for ty in (y0 / TILE)..=((y1 - 1) / TILE) {
        for tx in (x0 / TILE)..=((x1 - 1) / TILE) {
            if dirty[(ty * tiles_x + tx) as usize] {
                return true;
            }
        }
    }
    false
}

struct DisplayState {
    window: Window,
    width: i32,
    height: i32,
    /// static wallpaper composited on black, restored under moving particles
    base: Vec<Color>,
    particles: Vec<Particle>,
    dirty: Vec<bool>,
    tiles_x: i32,
    tiles_y: i32,
    rng: Rng,
    pending: Option<(u32, u32)>,
    smoke_target: usize,
    spark_cap: usize,
    cache_once: bool,
    frame: u32,
}

impl DisplayState {
    fn new_smoke(&mut self) -> Particle {
        let w = self.width as f32;
        let h = self.height as f32;
        let speed_scale = h / 600.0;
        let cw = self.rng.f();
        Particle {
            x: self.rng.range(-20.0, w + 20.0),
            y: h + self.rng.range(6.0, 40.0),
            vx: self.rng.range(-8.0, 8.0),
            vy: -self.rng.range(18.0, 42.0) * speed_scale,
            age: 0.0,
            life: self.rng.range(7.0, 13.0),
            seed: self.rng.range(0.0, 6.28318),
            color_i: if cw < 0.25 {
                0
            } else if cw < 0.60 {
                1
            } else {
                2
            },
            size_i: (self.rng.f() * 3.0) as usize,
            spark: false,
            prev_box: None,
        }
    }

    fn new_spark(&mut self) -> Particle {
        let w = self.width as f32;
        let h = self.height as f32;
        let speed_scale = h / 600.0;
        Particle {
            x: self.rng.range(0.0, w),
            y: h - self.rng.range(0.0, 40.0),
            vx: self.rng.range(-22.0, 22.0),
            vy: -self.rng.range(70.0, 150.0) * speed_scale,
            age: 0.0,
            life: self.rng.range(1.2, 2.8),
            seed: self.rng.range(0.0, 6.28318),
            color_i: (self.rng.f() * SPARK_COLORS.len() as f32) as usize,
            size_i: (self.rng.f() * SPARK_SIZES.len() as f32) as usize,
            spark: true,
            prev_box: None,
        }
    }

    /// One physics tick: drift, turbulence, cull, respawn. No drawing.
    /// Sparks advance every frame; the slow smoke only on smoke ticks (15 Hz).
    fn simulate(&mut self, smoke_tick: bool) {
        let w = self.width as f32;
        let h = self.height as f32;
        for p in &mut self.particles {
            if p.spark {
                p.age += FRAME_DT;
                p.vx += (p.age * 7.0 + p.seed).sin() * 30.0 * FRAME_DT;
                p.x += p.vx * FRAME_DT;
                p.y += p.vy * FRAME_DT;
            } else if smoke_tick {
                p.age += SMOKE_DT;
                let wob =
                    (p.age * 0.8 + p.seed).sin() + 0.5 * (p.age * 2.3 + p.seed * 1.7).sin();
                p.x += wob * 6.0 * SMOKE_DT;
                p.vy -= 2.0 * SMOKE_DT; // slight buoyancy
                p.x += p.vx * SMOKE_DT;
                p.y += p.vy * SMOKE_DT;
            }
        }
        self.particles.retain(|p| {
            p.age < p.life
                && p.x > -80.0
                && p.x < w + 80.0
                && p.y > if p.spark { 0.02 * h } else { 0.36 * h }
        });

        if smoke_tick {
            let mut smoke_n = self.particles.iter().filter(|p| !p.spark).count();
            let mut budget = 3;
            while smoke_n < self.smoke_target && budget > 0 {
                let p = self.new_smoke();
                self.particles.push(p);
                smoke_n += 1;
                budget -= 1;
            }
        }
        let spark_n = self.particles.iter().filter(|p| p.spark).count();
        if spark_n < self.spark_cap && self.rng.f() < 0.35 {
            let p = self.new_spark();
            self.particles.push(p);
        }
    }

    fn warmup(&mut self, frames: usize) {
        for i in 0..frames {
            self.simulate(i % 2 == 0);
        }
    }

    /// Rebuild the base image for the current size and repaint everything.
    fn rebuild(&mut self, path: &str, mode: BackgroundMode, should_cache: bool) {
        let w = self.width as u32;
        let h = self.height as u32;
        let mut base = vec![Color::rgb(4, 2, 2); (w * h) as usize];
        match scale_and_cache(path, mode, w, h, should_cache) {
            Ok(img) => {
                let iw = img.width();
                let ih = img.height();
                let (crop_x, crop_w) = if iw > w { ((iw - w) / 2, w) } else { (0, iw) };
                let (crop_y, crop_h) = if ih > h { ((ih - h) / 2, h) } else { (0, ih) };
                let dx = ((w - crop_w) / 2) as usize;
                let dy = ((h - crop_h) / 2) as usize;
                let data = img.data();
                for row in 0..crop_h as usize {
                    let s = (crop_y as usize + row) * iw as usize + crop_x as usize;
                    let d = (dy + row) * w as usize + dx;
                    base[d..d + crop_w as usize].copy_from_slice(&data[s..s + crop_w as usize]);
                }
            }
            Err(err) => error!("error loading {}: {}", path, err),
        }
        self.base = base;
        self.tiles_x = (self.width + TILE - 1) / TILE;
        self.tiles_y = (self.height + TILE - 1) / TILE;
        self.dirty = vec![false; (self.tiles_x * self.tiles_y) as usize];
        self.smoke_target = (((w * h) as f32 / (800.0 * 600.0)) * 150.0).min(450.0) as usize;
        for p in &mut self.particles {
            p.prev_box = None;
        }

        let data = self.window.data_mut();
        let n = data.len().min(self.base.len());
        data[..n].copy_from_slice(&self.base[..n]);
        self.window.sync();
    }

    /// One animation frame: apply pending resize, restore dirty tiles from the
    /// base image, advance the simulation, blend sprites, send merged damage.
    fn step(&mut self, bank: &SpriteBank, path: &str, mode: BackgroundMode) {
        if let Some((w, h)) = self.pending.take() {
            self.width = w as i32;
            self.height = h as i32;
            let cache = self.cache_once;
            self.cache_once = false;
            self.rebuild(path, mode, cache);
        }
        if self.base.is_empty() {
            return;
        }
        let h = self.height as f32;
        self.frame = self.frame.wrapping_add(1);
        let smoke_tick = self.frame % 2 == 0;

        // Only particles that advance this frame invalidate their old boxes;
        // resting smoke keeps its pixels and needs neither restore nor redraw.
        for p in &self.particles {
            if p.spark || smoke_tick {
                if let Some(b) = p.prev_box {
                    mark_tiles(&mut self.dirty, self.tiles_x, self.width, self.height, b);
                }
            }
        }

        self.simulate(smoke_tick);

        for p in &mut self.particles {
            if !(p.spark || smoke_tick) {
                continue;
            }
            let b = sprite_of(bank, p, h).map(|spr| {
                (
                    (p.x - spr.w as f32 / 2.0).round() as i32,
                    (p.y - spr.h as f32 / 2.0).round() as i32,
                    spr.w,
                    spr.h,
                )
            });
            if let Some(rect) = b {
                mark_tiles(&mut self.dirty, self.tiles_x, self.width, self.height, rect);
            }
            p.prev_box = b;
        }

        let mut rects: Vec<(i32, i32, u32, u32)> = Vec::new();
        {
            let width = self.width;
            let height = self.height;
            let data = self.window.data_mut();
            if data.len() < (width * height) as usize
                || self.base.len() < (width * height) as usize
            {
                // buffer mid-resize; dirty marks survive to the next frame
                return;
            }
            for ty in 0..self.tiles_y {
                let mut tx = 0;
                while tx < self.tiles_x {
                    if !self.dirty[(ty * self.tiles_x + tx) as usize] {
                        tx += 1;
                        continue;
                    }
                    let start = tx;
                    while tx < self.tiles_x && self.dirty[(ty * self.tiles_x + tx) as usize] {
                        tx += 1;
                    }
                    let x = start * TILE;
                    let y = ty * TILE;
                    let rw = ((tx - start) * TILE).min(width - x);
                    let rh = TILE.min(height - y);
                    for row in y..y + rh {
                        let o = (row * width + x) as usize;
                        data[o..o + rw as usize]
                            .copy_from_slice(&self.base[o..o + rw as usize]);
                    }
                    rects.push((x, y, rw as u32, rh as u32));
                }
            }
            // Re-blend every particle whose pixels were disturbed by the
            // restore, including resting smoke crossed by sparks.
            for p in &self.particles {
                if let Some((bx, by, bw, bh)) = p.prev_box {
                    if hits_dirty(&self.dirty, self.tiles_x, width, height, (bx, by, bw, bh)) {
                        if let Some(spr) = sprite_of(bank, p, h) {
                            blend_sprite(data, width, height, spr, bx, by);
                        }
                    }
                }
            }
        }
        self.dirty.fill(false);
        if !rects.is_empty() {
            self.window.update_rects(&rects);
        }
    }

    fn on_window_event(&mut self) {
        let mut new_size = None;
        for event in self.window.events() {
            match event.to_option() {
                EventOption::Resize(resize_event) => {
                    new_size = Some((resize_event.width, resize_event.height));
                }
                EventOption::Screen(screen_event) => {
                    self.window.set_size(screen_event.width, screen_event.height);
                    new_size = Some((screen_event.width, screen_event.height));
                }
                _ => (),
            }
        }
        if new_size.is_some() {
            self.pending = new_size;
        }
    }
}

fn main() {
    // Ignore possible errors while enabling logging
    let _ = RedoxLogger::new()
        .with_output(
            OutputBuilder::stdout()
                .with_filter(log::LevelFilter::Warn)
                .with_ansi_escape_codes()
                .build(),
        )
        .with_process_name("background".into())
        .enable();

    let mut args = env::args().skip(1);

    let path = match args.next() {
        Some(arg) => arg,
        None => find_background(),
    };

    let mode = BackgroundMode::from_str(&args.next().unwrap_or_default());

    let bank = SpriteBank::new();

    let seed = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0x9E37_79B9)
        ^ std::process::id();

    let mut displays = Vec::new();
    for (i, display) in get_display_rects()
        .expect("background: failed to get display rects")
        .into_iter()
        .enumerate()
    {
        let window = Window::new_flags(
            display.x,
            display.y,
            display.width,
            display.height,
            "",
            &[
                WindowFlag::Async,
                WindowFlag::Back,
                WindowFlag::Borderless,
                WindowFlag::Unclosable,
            ],
        )
        .unwrap();

        displays.push(DisplayState {
            window,
            width: display.width as i32,
            height: display.height as i32,
            base: Vec::new(),
            particles: Vec::new(),
            dirty: Vec::new(),
            tiles_x: 0,
            tiles_y: 0,
            rng: Rng::new(seed.wrapping_mul(i as u32 + 1).wrapping_add(0x9E37_79B9)),
            pending: Some((display.width, display.height)),
            smoke_target: 0,
            spark_cap: 16,
            cache_once: i == 0,
            frame: 0,
        });
    }

    // First frame draws the wallpaper; the warmup develops the smoke field so
    // the desktop never starts empty.
    for display in displays.iter_mut() {
        display.step(&bank, &path, mode);
        display.warmup(500);
        display.step(&bank, &path, mode);
    }

    if let Err(err) = remove_unused_cache() {
        warn!("Unable to clear background cache {:?}", err);
    }

    warn!("E-OS animated background: frame loop start");

    // Plain frame loop: async windows drain their events without blocking, so
    // one sleep paces both the simulation and input handling.
    let frame = Duration::from_millis(FRAME_MS);
    let idle_floor = Duration::from_millis(10);
    loop {
        let t0 = Instant::now();
        for display in displays.iter_mut() {
            display.on_window_event();
            display.step(&bank, &path, mode);
        }
        // On slow hardware degrade the frame rate instead of busy-spinning.
        let elapsed = t0.elapsed();
        let idle = if elapsed >= frame {
            idle_floor
        } else {
            frame - elapsed
        };
        thread::sleep(idle);
    }
}
