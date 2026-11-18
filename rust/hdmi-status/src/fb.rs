use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;

pub const W: usize = 1920;
pub const H: usize = 1080;

pub struct Framebuffer {
    _file: File, // keep /dev/fb0 open so HiSilicon hifb layer stays visible
    ptr: *mut u8,
    smem: usize,
    pub stride: usize,
}

impl Framebuffer {
    pub fn open() -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        let fd = file.as_raw_fd();
        let mut v = [0u32; 40];
        unsafe {
            if libc::ioctl(fd, 0x4600, v.as_mut_ptr()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        v[0] = W as u32;
        v[1] = H as u32;
        v[2] = W as u32;
        v[3] = H as u32;
        v[4] = 0;
        v[5] = 0;
        v[6] = 32;
        v[7] = 0;
        // ARGB8888
        let fb_fmt = [16, 8, 0, 8, 8, 0, 0, 8, 0, 24, 8, 0];
        v[8..20].copy_from_slice(&fb_fmt);
        v[21] = 0;
        unsafe {
            if libc::ioctl(fd, 0x4601, v.as_ptr()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        let mut fix = [0u8; 80];
        unsafe {
            if libc::ioctl(fd, 0x4602, fix.as_mut_ptr()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        let smem = u32::from_le_bytes(fix[20..24].try_into().unwrap()) as usize;
        let stride = u32::from_le_bytes(fix[44..48].try_into().unwrap()) as usize;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                smem,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            _file: file,
            ptr: ptr as *mut u8,
            smem,
            stride,
        })
    }

    pub fn clear(&mut self, alpha: u8) {
        let len = (H * self.stride).min(self.smem);
        unsafe {
            std::ptr::write_bytes(self.ptr, alpha, len);
        }
    }

    pub fn present(&mut self, buf: &[u8], w: usize, h: usize) {
        if self.stride == w * 4 && buf.len() <= self.smem {
            unsafe {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), self.ptr, buf.len());
            }
        } else {
            let row_bytes = w * 4;
            for y in 0..h.min(H) {
                let dst_off = y * self.stride;
                let src_off = y * row_bytes;
                if dst_off + row_bytes <= self.smem && src_off + row_bytes <= buf.len() {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            buf.as_ptr().add(src_off),
                            self.ptr.add(dst_off),
                            row_bytes,
                        );
                    }
                }
            }
        }
    }

    pub fn blit_rect(&mut self, buf: &[u8], bw: usize, bh: usize, x: i32, y: i32) {
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = (x + bw as i32).min(W as i32).max(0) as usize;
        let y1 = (y + bh as i32).min(H as i32).max(0) as usize;
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let rw = (x1 - x0) * 4;
        let sx = (x0 as i32 - x) as usize;
        let sy = (y0 as i32 - y) as usize;
        for row in 0..(y1 - y0) {
            let dst_off = (y0 + row) * self.stride + x0 * 4;
            let src_off = ((sy + row) * bw + sx) * 4;
            if dst_off + rw <= self.smem && src_off + rw <= buf.len() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        buf.as_ptr().add(src_off),
                        self.ptr.add(dst_off),
                        rw,
                    );
                }
            }
        }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        self.clear(0);
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.smem);
        }
    }
}
