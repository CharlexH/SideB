use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;

use crate::constants::FB_SIZE;

pub struct Framebuffer {
    fb_ptr: *mut u8,
    _file: std::fs::File,
}

// Safety: The mmap'd memory is only accessed through &mut references
// protected by the render mutex in the main app.
unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    /// Open /dev/fb0 and mmap it.
    pub fn open() -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fb0")
            .map_err(|e| format!("open /dev/fb0: {e}"))?;

        let fd = file.as_raw_fd();
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                FB_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err("mmap failed".to_string());
        }

        Ok(Self {
            fb_ptr: ptr as *mut u8,
            _file: file,
        })
    }

    /// Copy entire back buffer to the visible framebuffer.
    pub fn swap_buffers(&self, back_buf: &[u8]) {
        unsafe {
            std::ptr::copy_nonoverlapping(back_buf.as_ptr(), self.fb_ptr, FB_SIZE);
        }
    }

    /// Copy a rectangular region from back buffer to framebuffer.
    pub fn copy_rect(
        &self,
        back_buf: &[u8],
        min_x: usize,
        min_y: usize,
        max_x: usize,
        max_y: usize,
    ) {
        let min_x = min_x.min(crate::constants::SCREEN_W);
        let max_x = max_x.min(crate::constants::SCREEN_W);
        let min_y = min_y.min(crate::constants::SCREEN_H);
        let max_y = max_y.min(crate::constants::SCREEN_H);
        if min_x >= max_x || min_y >= max_y {
            return;
        }
        let row_bytes = (max_x - min_x) * crate::constants::BPP;
        for y in min_y..max_y {
            let start = (y * crate::constants::SCREEN_W + min_x) * crate::constants::BPP;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    back_buf.as_ptr().add(start),
                    self.fb_ptr.add(start),
                    row_bytes,
                );
            }
        }
    }

    /// Get mutable slice to the framebuffer (for clearing on exit).
    pub fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.fb_ptr, FB_SIZE) }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.fb_ptr as *mut libc::c_void, FB_SIZE);
        }
    }
}
