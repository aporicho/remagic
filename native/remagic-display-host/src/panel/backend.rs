use super::{PanelBackend, RefreshIntent};
use crate::geometry::Rect;
use std::io;

/// Host-development backend used by protocol integration tests. It exercises
/// all surface, damage and input scheduling without claiming device hardware.
pub struct MemoryBackend {
    width: i32,
    height: i32,
    stride: usize,
    pixels: Vec<u8>,
    next_marker: u64,
    submissions: Vec<MemorySubmission>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemorySubmission {
    pub marker: u64,
    pub rect: Rect,
    pub intent: RefreshIntent,
}

impl MemoryBackend {
    pub fn new(width: i32, height: i32) -> io::Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid mock geometry",
            ));
        }
        let stride = width as usize * 4;
        Ok(Self {
            width,
            height,
            stride,
            pixels: vec![0xff; stride * height as usize],
            next_marker: 0,
            submissions: Vec::new(),
        })
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn submissions(&self) -> &[MemorySubmission] {
        &self.submissions
    }

    #[cfg(test)]
    pub(crate) fn clear_submissions(&mut self) {
        self.submissions.clear();
    }
}

impl PanelBackend for MemoryBackend {
    fn width(&self) -> i32 {
        self.width
    }

    fn height(&self) -> i32 {
        self.height
    }

    fn stride(&self) -> usize {
        self.stride
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    fn submit(&mut self, rect: Rect, intent: RefreshIntent) -> io::Result<u64> {
        self.next_marker = self.next_marker.wrapping_add(1).max(1);
        self.submissions.push(MemorySubmission {
            marker: self.next_marker,
            rect,
            intent,
        });
        Ok(self.next_marker)
    }
}

#[cfg(feature = "device")]
pub struct QuillBackend {
    width: i32,
    height: i32,
    stride: usize,
    pixels: *mut u8,
}

#[cfg(feature = "device")]
impl QuillBackend {
    pub fn open() -> io::Result<Self> {
        unsafe {
            if quill_init() != 0 {
                return Err(io::Error::other("quill initialization failed"));
            }
            let width = quill_width();
            let height = quill_height();
            let stride = quill_stride();
            let pixels = quill_buffer();
            if width <= 0 || height <= 0 || stride < width * 4 || pixels.is_null() {
                return Err(io::Error::other("quill returned an invalid framebuffer"));
            }
            Ok(Self {
                width,
                height,
                stride: stride as usize,
                pixels,
            })
        }
    }
}

#[cfg(feature = "device")]
impl PanelBackend for QuillBackend {
    fn width(&self) -> i32 {
        self.width
    }

    fn height(&self) -> i32 {
        self.height
    }

    fn stride(&self) -> usize {
        self.stride
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.stride * self.height as usize) }
    }

    fn submit(&mut self, rect: Rect, intent: RefreshIntent) -> io::Result<u64> {
        let marker = unsafe {
            match intent {
                RefreshIntent::Ink => quill_swap_mono_fast(rect.x, rect.y, rect.width, rect.height),
                RefreshIntent::MonoQuality => {
                    quill_swap_mono_quality(rect.x, rect.y, rect.width, rect.height)
                }
                RefreshIntent::Ui | RefreshIntent::Content => {
                    quill_swap_color(rect.x, rect.y, rect.width, rect.height)
                }
                RefreshIntent::Full => {
                    quill_swap_color_full(rect.x, rect.y, rect.width, rect.height)
                }
            }
        };
        valid_marker(marker as u64)
    }

    fn process_events(&mut self) {
        unsafe { quill_process_events() }
    }
}

#[cfg(any(feature = "device", test))]
fn valid_marker(marker: u64) -> io::Result<u64> {
    if marker == 0 {
        Err(io::Error::other("quill rejected the panel submission"))
    } else {
        Ok(marker)
    }
}

#[cfg(feature = "device")]
#[link(name = "quill")]
unsafe extern "C" {
    fn quill_init() -> libc::c_int;
    fn quill_width() -> libc::c_int;
    fn quill_height() -> libc::c_int;
    fn quill_stride() -> libc::c_int;
    fn quill_buffer() -> *mut u8;
    fn quill_swap_mono_fast(x: i32, y: i32, width: i32, height: i32) -> libc::c_ulong;
    fn quill_swap_mono_quality(x: i32, y: i32, width: i32, height: i32) -> libc::c_ulong;
    fn quill_swap_color(x: i32, y: i32, width: i32, height: i32) -> libc::c_ulong;
    fn quill_swap_color_full(x: i32, y: i32, width: i32, height: i32) -> libc::c_ulong;
    fn quill_process_events();
}

#[cfg(test)]
mod marker_tests {
    use super::valid_marker;

    #[test]
    fn zero_quill_marker_is_a_submission_failure() {
        assert!(valid_marker(0).is_err());
        assert_eq!(valid_marker(7).unwrap(), 7);
    }
}
