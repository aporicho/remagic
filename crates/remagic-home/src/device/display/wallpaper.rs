use super::Display;
use crate::device::settings::{HomeSettings, WallpaperFit, WallpaperOption};
use png::{BitDepth, ColorType, Transformations};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

const MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;

struct GrayImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
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

fn decode_png(path: &Path) -> io::Result<GrayImage> {
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
    let pixels = to_gray(source, output.color_type, width, height)?;
    Ok(GrayImage {
        width,
        height,
        pixels,
    })
}

fn to_gray(source: &[u8], color: ColorType, width: usize, height: usize) -> io::Result<Vec<u8>> {
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
            let (gray, alpha) = match color {
                ColorType::Grayscale => (pixel[0], 255),
                ColorType::GrayscaleAlpha => (pixel[0], pixel[1]),
                ColorType::Rgb => (luma(pixel[0], pixel[1], pixel[2]), 255),
                ColorType::Rgba => (luma(pixel[0], pixel[1], pixel[2]), pixel[3]),
                ColorType::Indexed => unreachable!(),
            };
            composite_white(gray, alpha)
        })
        .collect())
}

fn luma(red: u8, green: u8, blue: u8) -> u8 {
    ((red as u32 * 77 + green as u32 * 150 + blue as u32 * 29) >> 8) as u8
}

fn composite_white(gray: u8, alpha: u8) -> u8 {
    let alpha = alpha as u32;
    ((gray as u32 * alpha + 255 * (255 - alpha)) / 255) as u8
}

fn draw_scaled(display: &mut Display, image: &GrayImage, fit: WallpaperFit) {
    if image.width == 0 || image.height == 0 {
        return;
    }
    let target_w = display.width as f64;
    let target_h = display.height as f64;
    let scale_x = target_w / image.width as f64;
    let scale_y = target_h / image.height as f64;
    let scale = match fit {
        WallpaperFit::Cover => scale_x.max(scale_y),
        WallpaperFit::Contain => scale_x.min(scale_y),
    };
    let drawn_w = (image.width as f64 * scale).round().max(1.0) as i32;
    let drawn_h = (image.height as f64 * scale).round().max(1.0) as i32;
    let origin_x = (display.width - drawn_w) / 2;
    let origin_y = (display.height - drawn_h) / 2;
    let x0 = origin_x.max(0);
    let y0 = origin_y.max(0);
    let x1 = (origin_x + drawn_w).min(display.width);
    let y1 = (origin_y + drawn_h).min(display.height);
    for y in y0..y1 {
        let source_y = (((y - origin_y) as f64 / scale).floor() as usize).min(image.height - 1);
        for x in x0..x1 {
            let source_x = (((x - origin_x) as f64 / scale).floor() as usize).min(image.width - 1);
            let gray = quantize(image.pixels[source_y * image.width + source_x]);
            display.pixel(x, y, gray_color(gray));
        }
    }
}

fn quantize(gray: u8) -> u8 {
    ((gray as u16 * 15 + 127) / 255 * 17) as u8
}

fn gray_color(gray: u8) -> u32 {
    0xFF00_0000 | ((gray as u32) << 16) | ((gray as u32) << 8) | gray as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_is_composited_on_white_and_gray_is_quantized() {
        assert_eq!(composite_white(0, 0), 255);
        assert_eq!(composite_white(0, 255), 0);
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(0, 0, 0), 0);
        assert_eq!(quantize(0), 0);
        assert_eq!(quantize(255), 255);
    }

    #[test]
    fn rgba_conversion_keeps_one_pixel_per_source_pixel() {
        let pixels = to_gray(&[0, 0, 0, 255, 255, 255, 255, 0], ColorType::Rgba, 2, 1).unwrap();
        assert_eq!(pixels, vec![0, 255]);
    }
}
