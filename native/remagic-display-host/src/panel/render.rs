use crate::geometry::{Geometry, Rect};
use crate::input::PenTool;
use crate::protocol::PixelFormat;
use crate::surface::SharedSurface;

const MAGIC_PAPER_PEN_MIN_RADIUS: i32 = 2;
const MAGIC_PAPER_PEN_PRESSURE_SPAN: i32 = 3;
const MAGIC_PAPER_ERASER_RADIUS: i32 = 22;

#[derive(Clone, Copy, Debug)]
pub(super) struct LivePenPoint {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) radius: i32,
    pub(super) tool: PenTool,
}

pub(super) fn live_brush_radius(tool: PenTool, pressure: i32, pressure_max: i32) -> i32 {
    match tool {
        PenTool::Pen => {
            let pressure_max = pressure_max.max(1);
            MAGIC_PAPER_PEN_MIN_RADIUS
                + pressure.clamp(0, pressure_max) * MAGIC_PAPER_PEN_PRESSURE_SPAN / pressure_max
        }
        PenTool::Eraser => MAGIC_PAPER_ERASER_RADIUS,
    }
}

pub(super) fn live_segment_radius(
    tool: PenTool,
    desired_radius: i32,
    previous: Option<LivePenPoint>,
) -> i32 {
    if tool != PenTool::Pen {
        return desired_radius;
    }
    previous
        .filter(|point| point.tool == PenTool::Pen)
        .map_or(desired_radius, |point| {
            desired_radius.min(point.radius.saturating_add(1))
        })
}

pub(super) fn sampled_signature(pixels: &[u8], stride: usize, rect: Rect) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let y_end = rect.y.saturating_add(rect.height).max(rect.y);
    for y in (rect.y.max(0)..y_end).step_by(16) {
        let start = y as usize * stride + rect.x.max(0) as usize * 4;
        let end = (start + rect.width.max(0) as usize * 4).min(pixels.len());
        for byte in pixels
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .step_by(32)
        {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash ^ ((rect.width as u64) << 32) ^ rect.height as u64
}

pub(super) fn point_inside(rect: Rect, x: i32, y: i32) -> bool {
    x >= rect.x && y >= rect.y && x < rect.right() && y < rect.bottom()
}

pub(super) fn copy_surface_rect(
    surface: &SharedSurface,
    destination: &mut [u8],
    destination_stride: usize,
    geometry: Geometry,
    physical_rect: Rect,
) {
    let source = surface.bytes();
    for py in physical_rect.y..physical_rect.bottom() {
        for px in physical_rect.x..physical_rect.right() {
            let (sx, sy) = geometry.physical_to_logical_point(px, py);
            let src = sy as usize * surface.stride + sx as usize * surface.format.bytes_per_pixel();
            let dst = py as usize * destination_stride + px as usize * 4;
            let [b, g, r, a] = decode_pixel(surface.format, source, src);
            if dst + 3 < destination.len() {
                destination[dst] = b;
                destination[dst + 1] = g;
                destination[dst + 2] = r;
                destination[dst + 3] = a;
            }
        }
    }
}

fn decode_pixel(format: PixelFormat, source: &[u8], index: usize) -> [u8; 4] {
    match format {
        PixelFormat::Rgb565 => {
            let value = u16::from_le_bytes([source[index], source[index + 1]]);
            let r = (((value >> 11) & 0x1f) as u32 * 255 / 31) as u8;
            let g = (((value >> 5) & 0x3f) as u32 * 255 / 63) as u8;
            let b = ((value & 0x1f) as u32 * 255 / 31) as u8;
            [b, g, r, 0xff]
        }
        PixelFormat::Rgb888 => [source[index + 2], source[index + 1], source[index], 0xff],
        PixelFormat::Rgba8888 => [
            source[index + 2],
            source[index + 1],
            source[index],
            source[index + 3],
        ],
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_line(
    pixels: &mut [u8],
    stride: usize,
    width: i32,
    height: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    radius: i32,
    color: [u8; 4],
) -> Rect {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for step in 0..=steps {
        let x = x0 + (x1 - x0) * step / steps;
        let y = y0 + (y1 - y0) * step / steps;
        draw_disc(pixels, stride, width, height, x, y, radius, color);
    }
    Rect::new(
        x0.min(x1) - radius,
        y0.min(y1) - radius,
        (x1 - x0).abs() + radius * 2 + 1,
        (y1 - y0).abs() + radius * 2 + 1,
    )
    .clip(width, height)
}

#[allow(clippy::too_many_arguments)]
fn draw_disc(
    pixels: &mut [u8],
    stride: usize,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    radius: i32,
    color: [u8; 4],
) {
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            let px = x + dx;
            let py = y + dy;
            if px < 0 || py < 0 || px >= width || py >= height {
                continue;
            }
            let index = py as usize * stride + px as usize * 4;
            if index + 3 < pixels.len() {
                pixels[index..index + 4].copy_from_slice(&color);
            }
        }
    }
}
