use super::Display;
use crate::device::settings::{HomeSettings, WallpaperFit, WallpaperOption};
use png::{BitDepth, ColorType, Transformations};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

mod thumbnail_cache;

const MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct ColorImage {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 3]>,
}

pub(super) fn draw(display: &mut Display, settings: &HomeSettings, option: &WallpaperOption) {
    display.fill(super::WHITE);
    let Some(path) = option.path.as_deref() else {
        return;
    };
    match decode_png(path) {
        Ok(image) => draw_scaled(display, &image, settings.lock.fit),
        Err(error) => eprintln!(
            "remagic-home: wallpaper {} ignored: {error}",
            path.display()
        ),
    }
}

pub(super) fn decode_png(path: &Path) -> io::Result<ColorImage> {
    let mut decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let size = reader.output_buffer_size();
    if size == 0 || size > MAX_DECODED_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("decoded size {size} is outside the safe limit"),
        ));
    }
    let mut buffer = vec![0; size];
    let output = reader
        .next_frame(&mut buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if output.bit_depth != BitDepth::Eight {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "wallpaper did not decode to 8-bit pixels",
        ));
    }
    let width = output.width as usize;
    let height = output.height as usize;
    let source = &buffer[..output.buffer_size()];
    let pixels = to_rgb(source, output.color_type, width, height)?;
    Ok(ColorImage {
        width,
        height,
        pixels,
    })
}

fn to_rgb(
    source: &[u8],
    color: ColorType,
    width: usize,
    height: usize,
) -> io::Result<Vec<[u8; 3]>> {
    let channels = match color {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "indexed wallpaper was not expanded",
            ))
        }
    };
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "wallpaper is too large"))?;
    if source.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "wallpaper pixel buffer has an invalid length",
        ));
    }
    Ok(source
        .chunks_exact(channels)
        .map(|pixel| {
            let (red, green, blue, alpha) = match color {
                ColorType::Grayscale => (pixel[0], pixel[0], pixel[0], 255),
                ColorType::GrayscaleAlpha => (pixel[0], pixel[0], pixel[0], pixel[1]),
                ColorType::Rgb => (pixel[0], pixel[1], pixel[2], 255),
                ColorType::Rgba => (pixel[0], pixel[1], pixel[2], pixel[3]),
                ColorType::Indexed => unreachable!(),
            };
            [
                composite_white(red, alpha),
                composite_white(green, alpha),
                composite_white(blue, alpha),
            ]
        })
        .collect())
}

fn composite_white(channel: u8, alpha: u8) -> u8 {
    let alpha = alpha as u32;
    ((channel as u32 * alpha + 255 * (255 - alpha)) / 255) as u8
}

fn draw_scaled(display: &mut Display, image: &ColorImage, fit: WallpaperFit) {
    draw_scaled_in(display, image, fit, 0, 0, display.width, display.height)
}

fn draw_scaled_in(
    display: &mut Display,
    image: &ColorImage,
    fit: WallpaperFit,
    target_x: i32,
    target_y: i32,
    target_width: i32,
    target_height: i32,
) {
    if image.width == 0 || image.height == 0 {
        return;
    }
    let target_w = target_width as f64;
    let target_h = target_height as f64;
    let scale_x = target_w / image.width as f64;
    let scale_y = target_h / image.height as f64;
    let scale = match fit {
        WallpaperFit::Cover => scale_x.max(scale_y),
        WallpaperFit::Contain => scale_x.min(scale_y),
    };
    let drawn_w = (image.width as f64 * scale).round().max(1.0) as i32;
    let drawn_h = (image.height as f64 * scale).round().max(1.0) as i32;
    let origin_x = target_x + (target_width - drawn_w) / 2;
    let origin_y = target_y + (target_height - drawn_h) / 2;
    let x0 = origin_x.max(target_x);
    let y0 = origin_y.max(target_y);
    let x1 = (origin_x + drawn_w).min(target_x + target_width);
    let y1 = (origin_y + drawn_h).min(target_y + target_height);
    for y in y0..y1 {
        let source_y = (((y - origin_y) as f64 / scale).floor() as usize).min(image.height - 1);
        for x in x0..x1 {
            let source_x = (((x - origin_x) as f64 / scale).floor() as usize).min(image.width - 1);
            let [red, green, blue] = image.pixels[source_y * image.width + source_x];
            display.pixel(x, y, rgb_color(red, green, blue));
        }
    }
}

pub(super) fn draw_thumbnail(
    display: &mut Display,
    option: &WallpaperOption,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    display.rect(x, y, width, height, super::GRAY);
    let Some(path) = option.path.as_deref() else {
        display.rect(x + 4, y + 4, width - 8, height - 8, super::WHITE);
        return;
    };
    let cached = thumbnail_cache::path(path).ok();
    let cached = cached
        .as_deref()
        .filter(|path| std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()));
    let Some(cached) = cached else {
        return;
    };
    match decode_png(cached) {
        Ok(image) => draw_scaled_in(
            display,
            &image,
            WallpaperFit::Cover,
            x + 4,
            y + 4,
            width - 8,
            height - 8,
        ),
        Err(error) => eprintln!(
            "remagic-home: wallpaper thumbnail {} ignored: {error}",
            cached.display()
        ),
    }
}

pub(super) fn prepare_thumbnails(options: &[WallpaperOption]) {
    thumbnail_cache::prepare(options)
}

fn rgb_color(red: u8, green: u8, blue: u8) -> u32 {
    0xFF00_0000 | ((red as u32) << 16) | ((green as u32) << 8) | blue as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_is_composited_on_white() {
        assert_eq!(composite_white(0, 0), 255);
        assert_eq!(composite_white(0, 255), 0);
    }

    #[test]
    fn rgba_conversion_preserves_color_and_composites_transparency() {
        let pixels = to_rgb(&[255, 0, 0, 255, 0, 0, 255, 0], ColorType::Rgba, 2, 1).unwrap();
        assert_eq!(pixels, vec![[255, 0, 0], [255, 255, 255]]);
    }
}
