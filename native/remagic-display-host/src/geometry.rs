use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }

    pub fn clip(self, width: i32, height: i32) -> Self {
        let x0 = self.x.clamp(0, width);
        let y0 = self.y.clamp(0, height);
        let x1 = self.right().clamp(0, width);
        let y1 = self.bottom().clamp(0, height);
        Self::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }

    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn expand(self, amount: i32) -> Self {
        if self.is_empty() {
            return self;
        }
        Self::new(
            self.x.saturating_sub(amount),
            self.y.saturating_sub(amount),
            self.width.saturating_add(amount.saturating_mul(2)),
            self.height.saturating_add(amount.saturating_mul(2)),
        )
    }
}

/// Mapping between an application's logical canvas and the vendor auxiliary
/// framebuffer. Paper Pro Move currently exposes 954 logical columns and a
/// 960-column auxiliary image; using one mapping for pixels, input and damage
/// prevents the six-column drift seen in ad-hoc integrations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub logical_width: i32,
    pub logical_height: i32,
    pub physical_width: i32,
    pub physical_height: i32,
}

impl Geometry {
    pub fn new(
        logical_width: i32,
        logical_height: i32,
        physical_width: i32,
        physical_height: i32,
    ) -> Option<Self> {
        if [
            logical_width,
            logical_height,
            physical_width,
            physical_height,
        ]
        .iter()
        .any(|value| *value <= 0)
        {
            return None;
        }
        Some(Self {
            logical_width,
            logical_height,
            physical_width,
            physical_height,
        })
    }

    pub fn logical_to_physical_point(self, x: i32, y: i32) -> (i32, i32) {
        (
            scale_round(x, self.logical_width, self.physical_width)
                .clamp(0, self.physical_width - 1),
            scale_round(y, self.logical_height, self.physical_height)
                .clamp(0, self.physical_height - 1),
        )
    }

    pub fn physical_to_logical_point(self, x: i32, y: i32) -> (i32, i32) {
        (
            scale_round(x, self.physical_width, self.logical_width)
                .clamp(0, self.logical_width - 1),
            scale_round(y, self.physical_height, self.logical_height)
                .clamp(0, self.logical_height - 1),
        )
    }

    /// Damage uses floor for its leading edge and ceil for its trailing edge,
    /// so scaling can never omit a changed application pixel.
    pub fn logical_to_physical_rect(self, rect: Rect) -> Rect {
        let rect = rect.clip(self.logical_width, self.logical_height);
        if rect.is_empty() {
            return rect;
        }
        let x0 = scale_floor(rect.x, self.logical_width, self.physical_width);
        let y0 = scale_floor(rect.y, self.logical_height, self.physical_height);
        let x1 = scale_ceil(rect.right(), self.logical_width, self.physical_width);
        let y1 = scale_ceil(rect.bottom(), self.logical_height, self.physical_height);
        Rect::new(x0, y0, x1 - x0, y1 - y0).clip(self.physical_width, self.physical_height)
    }
}

fn scale_round(value: i32, source: i32, target: i32) -> i32 {
    let numerator = value as i64 * target as i64;
    ((numerator + source as i64 / 2) / source as i64) as i32
}

fn scale_floor(value: i32, source: i32, target: i32) -> i32 {
    (value as i64 * target as i64 / source as i64) as i32
}

fn scale_ceil(value: i32, source: i32, target: i32) -> i32 {
    ((value as i64 * target as i64 + source as i64 - 1) / source as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_edges_map_to_the_actual_aux_buffer() {
        let geometry = Geometry::new(954, 1696, 960, 1696).unwrap();
        assert_eq!(geometry.logical_to_physical_point(0, 0), (0, 0));
        assert_eq!(geometry.logical_to_physical_point(953, 1695), (959, 1695));
    }

    #[test]
    fn scaled_damage_never_loses_an_edge() {
        let geometry = Geometry::new(954, 1696, 960, 1696).unwrap();
        let mapped = geometry.logical_to_physical_rect(Rect::new(953, 100, 1, 1));
        assert_eq!(mapped.right(), 960);
        assert!(mapped.width >= 1);
    }

    #[test]
    fn clipping_and_union_are_stable() {
        assert_eq!(
            Rect::new(-10, 3, 20, 5).clip(100, 100),
            Rect::new(0, 3, 10, 5)
        );
        assert_eq!(
            Rect::new(2, 3, 4, 5).union(Rect::new(5, 1, 4, 4)),
            Rect::new(2, 1, 7, 7)
        );
    }
}
