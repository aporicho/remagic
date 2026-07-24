use super::{decode_png, ColorImage};
use crate::device::settings::WallpaperOption;
use png::{BitDepth, ColorType};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const CACHE_DIR: &str = "/home/root/.cache/remagic/wallpaper-thumbnails";
const EVENT_SOCKET: &str = "/run/remagic/home-events.sock";
const THUMBNAIL_EDGE: usize = 384;
static WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) fn prepare(options: &[WallpaperOption]) {
    if WORKER_ACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    let sources = options
        .iter()
        .filter_map(|option| option.path.clone())
        .collect::<Vec<_>>();
    std::thread::spawn(move || {
        let changed = prepare_all(&sources).unwrap_or_else(|error| {
            eprintln!("remagic-home: wallpaper thumbnail preparation failed: {error}");
            false
        });
        WORKER_ACTIVE.store(false, Ordering::Release);
        if changed {
            notify_home();
        }
    });
}

pub(super) fn path(source: &Path) -> io::Result<PathBuf> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "wallpaper source is not a regular file",
        ));
    }
    let modified = metadata.modified()?.duration_since(std::time::UNIX_EPOCH);
    let modified = modified.unwrap_or_default().as_nanos();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source.as_os_str().as_bytes() {
        hash = (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    for byte in metadata
        .len()
        .to_le_bytes()
        .into_iter()
        .chain(modified.to_le_bytes())
    {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(Path::new(CACHE_DIR).join(format!("{hash:016x}.png")))
}

fn prepare_all(sources: &[PathBuf]) -> io::Result<bool> {
    ensure_cache_directory()?;
    let mut changed = false;
    for source in sources {
        let destination = path(source)?;
        if regular_file(&destination) {
            continue;
        }
        match write_thumbnail(source, &destination) {
            Ok(()) => changed = true,
            Err(error) => eprintln!(
                "remagic-home: cannot cache wallpaper {}: {error}",
                source.display()
            ),
        }
    }
    Ok(changed)
}

fn ensure_cache_directory() -> io::Result<()> {
    match fs::symlink_metadata(CACHE_DIR) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "wallpaper thumbnail cache is not a directory",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(CACHE_DIR)?,
        Err(error) => return Err(error),
    }
    fs::set_permissions(CACHE_DIR, fs::Permissions::from_mode(0o700))
}

fn write_thumbnail(source: &Path, destination: &Path) -> io::Result<()> {
    let image = decode_png(source)?;
    let thumbnail = square_thumbnail(&image);
    let temporary = destination.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let mut encoder = png::Encoder::new(
            BufWriter::new(file),
            THUMBNAIL_EDGE as u32,
            THUMBNAIL_EDGE as u32,
        );
        encoder.set_color(ColorType::Rgb);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer
            .write_image_data(&thumbnail)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer
            .finish()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        File::open(&temporary)?.sync_all()?;
        fs::rename(&temporary, destination)?;
        File::open(CACHE_DIR)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn square_thumbnail(image: &ColorImage) -> Vec<u8> {
    let source_edge = image.width.min(image.height).max(1);
    let source_x = (image.width.saturating_sub(source_edge)) / 2;
    let source_y = (image.height.saturating_sub(source_edge)) / 2;
    let mut output = Vec::with_capacity(THUMBNAIL_EDGE * THUMBNAIL_EDGE * 3);
    for y in 0..THUMBNAIL_EDGE {
        let source_y = source_y + y * source_edge / THUMBNAIL_EDGE;
        for x in 0..THUMBNAIL_EDGE {
            let source_x = source_x + x * source_edge / THUMBNAIL_EDGE;
            output.extend_from_slice(&image.pixels[source_y * image.width + source_x]);
        }
    }
    output
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn notify_home() {
    let Ok(socket) = UnixDatagram::unbound() else {
        return;
    };
    let _ = socket.send_to(b"wallpapers_changed\n", Path::new(EVENT_SOCKET));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_thumbnail_center_crops_and_keeps_rgb_channels() {
        let image = ColorImage {
            width: 4,
            height: 2,
            pixels: vec![
                [1, 0, 0],
                [2, 0, 0],
                [3, 0, 0],
                [4, 0, 0],
                [5, 0, 0],
                [6, 0, 0],
                [7, 0, 0],
                [8, 0, 0],
            ],
        };
        let thumbnail = square_thumbnail(&image);
        assert_eq!(thumbnail.len(), THUMBNAIL_EDGE * THUMBNAIL_EDGE * 3);
        assert_eq!(&thumbnail[..3], &[2, 0, 0]);
        assert_eq!(&thumbnail[thumbnail.len() - 3..], &[7, 0, 0]);
    }
}
