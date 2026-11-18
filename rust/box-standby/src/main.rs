//! Real suspend-to-RAM for the CM101s-2 via /dev/hi_pm.
//! Usage: box-standby [wake_after_seconds]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::io::AsRawFd;
use std::time::Duration;

const POWER_KEY: u32 = 0xe718_fe01;
const PM_MAGIC: u32 = 0x62;
const VAL_SIZE: usize = 252; // C51_PMOC_VAL_S size in 4.4.35 kernel
const LOG_PATH: &str = "/var/log/box-standby.log";

const fn ioc(dir: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | (PM_MAGIC << 8) | nr) as libc::c_ulong
}

const GET_WAKEUP: libc::c_ulong = ioc(2, 0x0c, VAL_SIZE as u32);
const SET_WAKEUP: libc::c_ulong = ioc(1, 0x02, VAL_SIZE as u32);
const WAKEUP_READY: libc::c_ulong = ioc(0, 0x03, 0);

fn log_line(msg: &str) {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe { libc::localtime_r(&now, &mut tm) };
    let ts = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    );
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_PATH) {
        let _ = writeln!(f, "{ts} {msg}");
    }
}

fn pause_player() {
    let addr: SocketAddr = "127.0.0.1:8790".parse().unwrap();
    if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let _ = stream.write_all(b"GET /cmd?act=pause HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
        let mut buf = [0u8; 256];
        let _ = stream.read(&mut buf);
    }
}

fn write_u32_le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn main() {
    let timeout: u32 = env::args()
        .nth(1)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0xffff_ffff);

    let hi_pm = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/hi_pm")
        .expect("open /dev/hi_pm");
    let fd = hi_pm.as_raw_fd();

    let mut val = [0u8; VAL_SIZE];
    unsafe {
        if libc::ioctl(fd, GET_WAKEUP, val.as_mut_ptr()) < 0 {
            panic!("GET_WAKEUP failed: {}", std::io::Error::last_os_error());
        }
    }

    write_u32_le(&mut val, 0, 1);         // one IR key
    write_u32_le(&mut val, 4, POWER_KEY); // irlVal[0]
    write_u32_le(&mut val, 28, 0);        // irhVal[0]
    write_u32_le(&mut val, 56, timeout);  // wake timeout

    unsafe {
        if libc::ioctl(fd, SET_WAKEUP, val.as_ptr()) < 0 {
            panic!("SET_WAKEUP failed: {}", std::io::Error::last_os_error());
        }
    }

    pause_player();
    unsafe { libc::sync() };

    let timeout_desc = if timeout == 0xffff_ffff {
        "none".to_string()
    } else {
        format!("{timeout} s")
    };
    log_line(&format!(
        "standby (wake key 0x{POWER_KEY:08x}, timeout {timeout_desc})"
    ));

    unsafe {
        if libc::ioctl(fd, WAKEUP_READY) < 0 {
            panic!("WAKEUP_READY failed: {}", std::io::Error::last_os_error());
        }
    }

    fs::write("/sys/power/state", b"mem").expect("write /sys/power/state");
    log_line("woke up");
}
