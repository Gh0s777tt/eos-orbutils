//! Shared pieces of the launcher binaries (bar/chooser + desktop icons):
//! scale, paths, icon loading and app-package enumeration.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicIsize, Ordering};

use log::error;
use orbclient::image::Image;
use orbclient::Color;

use crate::package::Package;

pub static SCALE: AtomicIsize = AtomicIsize::new(1);

pub fn chooser_width() -> u32 {
    200 * SCALE.load(Ordering::Relaxed) as u32
}

pub fn font_size() -> i32 {
    16 * SCALE.load(Ordering::Relaxed) as i32
}

pub fn icon_size() -> i32 {
    48 * SCALE.load(Ordering::Relaxed) as i32
}

pub fn icon_small_size() -> i32 {
    32 * SCALE.load(Ordering::Relaxed) as i32
}

#[cfg(target_os = "redox")]
pub static UI_PATH: &'static str = "/usr/share/ui";

#[cfg(not(target_os = "redox"))]
pub static UI_PATH: &'static str = "ui";

pub fn exec_to_command(exec: &str, path_opt: Option<&str>) -> Option<Command> {
    let args_vec: Vec<String> = shlex::split(exec)?;
    let mut args = args_vec.iter();
    let mut command = Command::new(args.next()?);
    for arg in args {
        if arg.starts_with('%') {
            match arg.as_str() {
                "%f" | "%F" | "%u" | "%U" => {
                    if let Some(path) = &path_opt {
                        command.arg(path);
                    }
                }
                _ => {
                    log::warn!("unsupported Exec code {:?} in {:?}", arg, exec);
                    return None;
                }
            }
        } else {
            command.arg(arg);
        }
    }
    Some(command)
}

pub fn spawn_exec(exec: &str, path_opt: Option<&str>) {
    match exec_to_command(exec, path_opt) {
        Some(mut command) => match command.spawn() {
            Ok(_) => {}
            Err(err) => {
                error!("failed to launch {}: {}", exec, err);
            }
        },
        None => {
            error!("failed to parse {}", exec);
        }
    }
}

pub fn size_icon(icon: Image, small: bool) -> Image {
    let size = if small {
        icon_small_size()
    } else {
        icon_size()
    } as u32;
    if icon.width() == size && icon.height() == size {
        icon
    } else {
        icon.resize(size, size, orbclient::image::ResizeType::Lanczos3)
    }
}

pub fn load_icon<P: AsRef<Path>>(path: P) -> Option<Image> {
    let icon = Image::from_path(path).ok()?;
    Some(size_icon(icon, false))
}

pub fn load_icon_small<P: AsRef<Path>>(path: P) -> Option<Image> {
    let icon = Image::from_path(path).ok()?;
    Some(size_icon(icon, true))
}

lazy_static::lazy_static! {
    static ref USVG_OPTIONS: resvg::usvg::Options<'static> = {
        let mut opt = resvg::usvg::Options::default();
        opt.fontdb_mut().load_system_fonts();
        opt
    };
}

pub fn load_icon_svg<P: AsRef<Path>>(path: P, small: bool) -> Option<Image> {
    let tree = {
        let svg_data = std::fs::read(path).ok()?;
        resvg::usvg::Tree::from_data(&svg_data, &USVG_OPTIONS).ok()?
    };

    let pixmap_size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixmap_size.width(), pixmap_size.height())?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixmap.as_mut(),
    );

    let width = pixmap.width();
    let height = pixmap.height();
    let mut data = Vec::with_capacity(width as usize * height as usize);
    for rgba in pixmap.take().chunks_exact(4) {
        data.push(Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3]));
    }

    let icon = Image::from_data(width, height, data.into())?;
    Some(size_icon(icon, small))
}

#[cfg(not(target_os = "redox"))]
pub fn wait(status: &mut i32) -> std::io::Result<usize> {
    use std::io::{Error, ErrorKind};

    let pid = unsafe { libc::waitpid(0, status as *mut i32, libc::WNOHANG) };
    if pid < 0 {
        let err = Error::last_os_error();
        if err.raw_os_error() == Some(libc::ECHILD) {
            return Ok(0);
        }
        return Err(std::io::Error::new(
            ErrorKind::Other,
            format!("waitpid failed: {}", err),
        ));
    }
    Ok(pid as usize)
}

#[cfg(target_os = "redox")]
pub fn wait(status: &mut i32) -> std::io::Result<usize> {
    use std::io::ErrorKind;

    match libredox::call::waitpid(0, status, libc::WNOHANG) {
        Ok(t) => Ok(t),
        Err(err) => {
            if err.errno() == libredox::errno::ECHILD {
                return Ok(0);
            }
            Err(std::io::Error::new(
                ErrorKind::Other,
                format!("Error in waitpid(): {}", err.to_string()),
            ))
        }
    }
}

pub fn get_packages() -> Vec<Package> {
    let mut packages: Vec<Package> = Vec::new();

    if let Ok(read_dir) = Path::new(&format!("{}/apps/", UI_PATH)).read_dir() {
        for entry_res in read_dir {
            let entry = match entry_res {
                Ok(x) => x,
                Err(_) => continue,
            };
            if entry
                .file_type()
                .expect("failed to get file_type")
                .is_file()
            {
                packages.push(Package::from_path(&entry.path().display().to_string()));
            }
        }
    }

    if let Ok(xdg_dirs) = xdg::BaseDirectories::new() {
        for path in xdg_dirs.find_data_files("applications") {
            if let Ok(read_dir) = path.read_dir() {
                for dir_entry_res in read_dir {
                    let Ok(dir_entry) = dir_entry_res else {
                        continue;
                    };
                    let Ok(id) = dir_entry.file_name().into_string() else {
                        continue;
                    };
                    if let Some(package) = Package::from_desktop_entry(id, &dir_entry.path()) {
                        packages.push(package);
                    }
                }
            }
        }
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    packages
}
