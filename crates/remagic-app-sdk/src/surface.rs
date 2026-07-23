pub type Rgb565 = u16;
pub const WHITE: Rgb565 = 0xffff;
pub const BLACK: Rgb565 = 0x0000;

pub struct Surface<'a> {
    bytes: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
}

impl<'a> Surface<'a> {
    pub fn new(bytes: &'a mut [u8], width: usize, height: usize, stride: usize) -> Option<Self> {
        let required = stride.checked_mul(height)?;
        (stride >= width.checked_mul(2)? && bytes.len() >= required).then_some(Self {
            bytes,
            width,
            height,
            stride,
        })
    }

    pub const fn width(&self) -> usize {
        self.width
    }

    pub const fn height(&self) -> usize {
        self.height
    }

    pub fn clear(&mut self, color: Rgb565) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Rgb565) {
        let x1 = x.saturating_add(width).min(self.width);
        let y1 = y.saturating_add(height).min(self.height);
        let pixel = color.to_le_bytes();
        for row in y.min(self.height)..y1 {
            let start = row * self.stride + x.min(self.width) * 2;
            let end = row * self.stride + x1 * 2;
            for bytes in self.bytes[start..end].chunks_exact_mut(2) {
                bytes.copy_from_slice(&pixel);
            }
        }
    }

    pub fn stroke_rect(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        thickness: usize,
        color: Rgb565,
    ) {
        self.fill_rect(x, y, width, thickness, color);
        self.fill_rect(
            x,
            y.saturating_add(height.saturating_sub(thickness)),
            width,
            thickness,
            color,
        );
        self.fill_rect(x, y, thickness, height, color);
        self.fill_rect(
            x.saturating_add(width.saturating_sub(thickness)),
            y,
            thickness,
            height,
            color,
        );
    }

    pub fn put_pixel(&mut self, x: i32, y: i32, color: Rgb565) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let offset = y as usize * self.stride + x as usize * 2;
        self.bytes[offset..offset + 2].copy_from_slice(&color.to_le_bytes());
    }

    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Rgb565, alpha: u8) {
        if alpha == 0 || x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        if alpha == u8::MAX {
            self.put_pixel(x, y, color);
            return;
        }
        let offset = y as usize * self.stride + x as usize * 2;
        let old = u16::from_le_bytes(self.bytes[offset..offset + 2].try_into().unwrap());
        let (dr, dg, db) = unpack(old);
        let (sr, sg, sb) = unpack(color);
        let mix = |destination: u8, source: u8| {
            ((destination as u16 * (255 - alpha) as u16 + source as u16 * alpha as u16 + 127) / 255)
                as u8
        };
        let blended = pack(mix(dr, sr), mix(dg, sg), mix(db, sb));
        self.bytes[offset..offset + 2].copy_from_slice(&blended.to_le_bytes());
    }
}

fn unpack(color: Rgb565) -> (u8, u8, u8) {
    (
        ((((color >> 11) & 0x1f) as u32 * 255 + 15) / 31) as u8,
        ((((color >> 5) & 0x3f) as u32 * 255 + 31) / 63) as u8,
        (((color & 0x1f) as u32 * 255 + 15) / 31) as u8,
    )
}

fn pack(red: u8, green: u8, blue: u8) -> Rgb565 {
    ((red as u16 >> 3) << 11) | ((green as u16 >> 2) << 5) | (blue as u16 >> 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_surfaces_and_clips_rectangles() {
        assert!(Surface::new(&mut [0; 3], 2, 1, 4).is_none());
        let mut bytes = [0_u8; 16];
        let mut surface = Surface::new(&mut bytes, 4, 2, 8).unwrap();
        surface.clear(WHITE);
        surface.fill_rect(3, 1, 9, 9, BLACK);
        assert_eq!(&bytes[14..16], &[0, 0]);
        assert_eq!(&bytes[12..14], &[255, 255]);
    }
}
