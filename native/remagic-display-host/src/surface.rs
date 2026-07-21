use crate::geometry::Rect;
use crate::protocol::PixelFormat;
use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

pub struct SharedSurface {
    pub key: i32,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub format: PixelFormat,
    pub shm_key: i32,
    pub len: usize,
    fd: RawFd,
    ptr: NonNull<u8>,
    name: CString,
    refresh_mode: AtomicI32,
    commit_sequence: AtomicU64,
}

unsafe impl Send for SharedSurface {}
unsafe impl Sync for SharedSurface {}

impl SharedSurface {
    pub fn create(
        key: i32,
        width: i32,
        height: i32,
        format: PixelFormat,
        shm_key: i32,
    ) -> io::Result<Self> {
        if width <= 0 || height <= 0 || width > 8192 || height > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid surface dimensions",
            ));
        }
        let stride = (width as usize)
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "surface stride overflow")
            })?;
        let len = stride
            .checked_mul(height as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "surface size overflow"))?;
        let name = CString::new(format!("/qtfb_{shm_key}"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid shm name"))?;
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                libc::S_IRUSR | libc::S_IWUSR,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
            }
            return Err(error);
        }
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
            }
            return Err(error);
        }
        let Some(ptr) = NonNull::new(raw.cast()) else {
            unsafe {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
            }
            return Err(io::Error::other("mmap returned a null pointer"));
        };
        unsafe { std::ptr::write_bytes(ptr.as_ptr(), 0xff, len) };
        Ok(Self {
            key,
            width,
            height,
            stride,
            format,
            shm_key,
            len,
            fd,
            ptr,
            name,
            refresh_mode: AtomicI32::new(crate::protocol::REFRESH_MODE_UI),
            commit_sequence: AtomicU64::new(0),
        })
    }

    pub fn refresh_mode(&self) -> i32 {
        self.refresh_mode.load(Ordering::Acquire)
    }

    pub fn set_refresh_mode(&self, mode: i32) {
        self.refresh_mode.store(mode, Ordering::Release);
    }

    pub fn mark_commit(&self) -> u64 {
        self.commit_sequence.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn commit_sequence(&self) -> u64 {
        self.commit_sequence.load(Ordering::Acquire)
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    pub fn full_rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

impl Drop for SharedSurface {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
            libc::close(self.fd);
            libc::shm_unlink(self.name.as_ptr());
        }
    }
}
