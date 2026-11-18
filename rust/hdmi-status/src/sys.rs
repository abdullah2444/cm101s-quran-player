use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::net::UdpSocket;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct Stats {
    cpu_prev: Option<(u64, u64)>,
    net_prev: Option<(Instant, u64, u64)>,
    svc: Vec<String>,
    svc_t: Option<Instant>,
}

impl Stats {
    pub fn new() -> Self {
        let mut s = Self {
            cpu_prev: None,
            net_prev: None,
            svc: Vec::new(),
            svc_t: None,
        };
        let _ = s.cpu();
        let _ = s.net();
        s
    }

    pub fn cpu(&mut self) -> f64 {
        let Ok(raw) = fs::read_to_string("/proc/stat") else {
            return 0.0;
        };
        let Some(first) = raw.lines().next() else {
            return 0.0;
        };
        let nums: Vec<u64> = first
            .split_whitespace()
            .skip(1)
            .filter_map(|x| x.parse().ok())
            .collect();
        if nums.len() < 5 {
            return 0.0;
        }
        let idle = nums[3] + nums[4];
        let total: u64 = nums.iter().sum();
        let pct = if let Some((pi, pt)) = self.cpu_prev {
            let dt = total.saturating_sub(pt);
            if dt > 0 {
                100.0 * (1.0 - (idle.saturating_sub(pi) as f64) / (dt as f64))
            } else {
                0.0
            }
        } else {
            0.0
        };
        self.cpu_prev = Some((idle, total));
        pct.clamp(0.0, 100.0)
    }

    pub fn net(&mut self) -> (f64, f64) {
        let mut rx = 0u64;
        let mut tx = 0u64;
        if let Ok(raw) = fs::read_to_string("/proc/net/dev") {
            for line in raw.lines().skip(2) {
                if let Some((name, rest)) = line.split_once(':') {
                    let iface = name.trim();
                    if iface == "eth0" || iface == "wlan0" {
                        let cols: Vec<&str> = rest.split_whitespace().collect();
                        if cols.len() >= 9 {
                            rx += cols[0].parse::<u64>().unwrap_or(0);
                            tx += cols[8].parse::<u64>().unwrap_or(0);
                        }
                    }
                }
            }
        }
        let now = Instant::now();
        let rates = if let Some((pt, prx, ptx)) = self.net_prev {
            let dt = now.duration_since(pt).as_secs_f64();
            if dt > 0.0 {
                (
                    rx.saturating_sub(prx) as f64 / dt,
                    tx.saturating_sub(ptx) as f64 / dt,
                )
            } else {
                (0.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        };
        self.net_prev = Some((now, rx, tx));
        rates
    }

    pub fn services(&mut self) -> &[String] {
        let refresh = match self.svc_t {
            None => true,
            Some(t) => Instant::now().duration_since(t) > Duration::from_secs(30),
        };
        if refresh {
            self.svc_t = Some(Instant::now());
            if let Ok(out) = Command::new("/usr/local/sbin/panel-svc").arg("list").output() {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let mut list = Vec::new();
                    for line in text.lines() {
                        let p: Vec<&str> = line.split('|').collect();
                        if p.len() == 5 && p[3] != "off" && p[0] != "hdmi" {
                            let label = match p[0] {
                                "aria2" => "Aria2",
                                "cron" => "Schedules",
                                "avahi" => "mDNS",
                                "samba" => "File sharing",
                                "ftp" => "FTP",
                                "smart" => "Disk health",
                                "rootreset" => "Root reset (!)",
                                "frpc" => "FRP tunnels",
                                _ => p[1],
                            };
                            list.push(label.to_string());
                        }
                    }
                    self.svc = list;
                }
            }
        }
        &self.svc
    }
}

pub fn pm_temp() -> Option<i32> {
    let raw = fs::read_to_string("/proc/msp/pm_cpu").ok()?;
    let idx = raw.find("temperature")?;
    let after = &raw[idx..];
    let eq = after.find('=')?;
    let tok = after[eq + 1..].split_whitespace().next()?;
    tok.parse().ok()
}

pub fn cpu_freq_str() -> String {
    let Ok(raw) = fs::read_to_string("/proc/msp/pm_cpu") else {
        return String::new();
    };
    let Some(idx) = raw.find("freq") else {
        return String::new();
    };
    let after = &raw[idx..];
    let Some(eq) = after.find('=') else {
        return String::new();
    };
    let Some(tok) = after[eq + 1..].split_whitespace().next() else {
        return String::new();
    };
    if let Ok(khz) = tok.parse::<f64>() {
        format!("{:.1} GHz", khz / 1e6)
    } else {
        String::new()
    }
}

pub fn mem() -> (u64, u64) {
    let mut d = HashMap::new();
    if let Ok(raw) = fs::read_to_string("/proc/meminfo") {
        for line in raw.lines() {
            let mut it = line.split_whitespace();
            if let (Some(k), Some(v)) = (it.next(), it.next()) {
                if let Ok(num) = v.parse::<u64>() {
                    d.insert(k.trim_end_matches(':').to_string(), num);
                }
            }
        }
    }
    let tot = *d.get("MemTotal").unwrap_or(&1);
    let avail = *d.get("MemAvailable").unwrap_or(&0);
    (tot.saturating_sub(avail), tot)
}

pub fn disks() -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let Ok(raw) = fs::read_to_string("/proc/mounts") else {
        return out;
    };
    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let dev = parts[0];
        let mnt = parts[1].replace("\\040", " ");
        if mnt == "/" || dev.starts_with("/dev/sd") || dev.starts_with("/dev/mmcblk1") {
            if !seen.insert(dev.to_string()) {
                continue;
            }
            let Ok(c_mnt) = CString::new(mnt.as_str()) else {
                continue;
            };
            let mut sv: libc::statvfs = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::statvfs(c_mnt.as_ptr(), &mut sv) };
            if rc != 0 {
                continue;
            }
            let tot = (sv.f_blocks as u64) * (sv.f_frsize as u64);
            if tot > 0 {
                let used = (sv.f_blocks.saturating_sub(sv.f_bfree) as u64) * (sv.f_frsize as u64);
                let name = if mnt == "/" {
                    "System".to_string()
                } else {
                    Path::new(&mnt)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&mnt)
                        .to_string()
                };
                out.push((name, used, tot));
                if out.len() >= 3 {
                    break;
                }
            }
        }
    }
    out
}

pub fn ip_addr() -> String {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
        return "no network".to_string();
    };
    if sock.connect("192.0.2.1:9").is_err() {
        return "no network".to_string();
    }
    sock.local_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "no network".to_string())
}

pub fn uptime_str() -> String {
    let s = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|raw| raw.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let d = s / 86400;
    let h = (s % 86400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

pub fn size_str(mut n: f64) -> String {
    for u in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 || u == "TB" {
            return if u == "B" || u == "KB" {
                format!("{n:.0} {u}")
            } else {
                format!("{n:.1} {u}")
            };
        }
        n /= 1024.0;
    }
    String::new()
}

pub fn rate_str(n: f64) -> String {
    format!("{}/s", size_str(n))
}
