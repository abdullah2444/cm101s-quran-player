use crate::{
    airplay, quran_hide, quran_hidden, screen_off, toast, unix_now, video, Shared, SCREEN_OFF,
    STANDBY_BIN, STANDBY_PY,
};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const IR_LOG: &str = "/run/ir-keys.log";

#[derive(Default)]
pub struct PowerState {
    pub busy: bool,
    pub woke: f64,
}

#[derive(Default)]
pub struct DigitState {
    pub buf: String,
    pub gen: u64,
}

#[derive(Default)]
struct GateState {
    key: Option<&'static str>,
    down: f64,
    last: f64,
}

fn map_key(code: &str) -> Option<&'static str> {
    Some(match code {
        "01fe46b9" => "up",
        "01fe16e9" => "down",
        "01fe47b8" => "left",
        "01fe15ea" => "right",
        "01fe55aa" => "ok",
        "01fe40bf" => "back",
        "01fe06f9" => "home",
        "01fe17e8" => "menu",
        "01fe1ae5" => "red",
        "01fe3cc3" => "green",
        "01fe14eb" | "01fef40b" => "voldown",
        "01fe10ef" | "01fef50a" => "volup",
        "01fef20d" => "set",
        "01fef30c" => "tvin",
        "01fe08f7" => "mute",
        "01fe4bb4" => "del",
        "01fe04fb" => "mouse",
        "01fe18e7" => "power_red",
        "01fef10e" => "power",
        "01fe0cf3" => "0",
        "01fe54ab" => "1",
        "01fe48b7" => "2",
        "01fe07f8" => "3",
        "01fe50af" => "4",
        "01fe12ed" => "5",
        "01fe11ee" => "6",
        "01fe4cb3" => "7",
        "01fe0ef1" => "8",
        "01fe0df2" => "9",
        _ => return None,
    })
}

fn key_gate(g: &mut GateState, k: &'static str, hold: bool) -> bool {
    let now = unix_now();
    if !hold {
        if g.key == Some(k) && now - g.last < 0.25 {
            g.last = now;
            return false;
        }
        g.key = Some(k);
        g.down = now;
        g.last = now;
        true
    } else if g.key != Some(k) || now - g.down < 0.6 || now - g.last < 0.25 {
        false
    } else {
        g.last = now;
        true
    }
}

fn standby(shared: &Arc<Shared>) {
    {
        let _vg = shared.vlock.lock().unwrap();
        airplay::airplay_stop_locked(shared);
        video::video_stop_locked(shared, true);
    }
    video::menu_close(shared);
    shared.quran_stop();
    quran_hide(false);
    toast("Turning off…");
    thread::sleep(Duration::from_millis(1200));
    let bin = if Path::new(STANDBY_BIN).exists() {
        STANDBY_BIN
    } else {
        STANDBY_PY
    };
    let _ = Command::new(bin).status();
    let mut pw = shared.power.lock().unwrap();
    pw.woke = unix_now();
    pw.busy = false;
}

fn digits_done(shared: &Shared, gen: u64, delay_ms: u64) {
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }
    let buf = {
        let mut d = shared.digits.lock().unwrap();
        if d.gen != gen || d.buf.is_empty() {
            return;
        }
        let s = d.buf.clone();
        d.buf.clear();
        s
    };
    let n: u32 = buf.parse().unwrap_or(0);
    if (1..=114).contains(&n) {
        let mut q = BTreeMap::new();
        q.insert("act".to_string(), "play".to_string());
        q.insert("surah".to_string(), n.to_string());
        shared.cmd(&q);
        let name = shared
            .chapters
            .get(&n)
            .map(|c| c.name.as_str())
            .unwrap_or("");
        toast(&format!("{n}. {name}"));
    } else {
        toast(&format!("There is no surah {n}"));
    }
}

fn remote_key(shared: &Arc<Shared>, k: &str, hold: bool) {
    if airplay::airplay_active(shared) && !hold {
        if matches!(k, "back" | "home" | "power") {
            let _vg = shared.vlock.lock().unwrap();
            airplay::airplay_stop_locked(shared);
            toast("AirPlay stopped");
            return;
        }
        if matches!(k, "red" | "tvin" | "green" | "ok" | "set" | "power_red") {
            let _vg = shared.vlock.lock().unwrap();
            airplay::airplay_stop_locked(shared);
        }
    }

    {
        let mn = shared.menu.lock().unwrap();
        if mn.on && unix_now() - mn.tk > 180.0 {
            drop(mn);
            video::menu_close(shared);
        }
    }

    let video_playing = shared.video.lock().unwrap().child.is_some();
    let menu_on = shared.menu.lock().unwrap().on;
    if hold
        && !matches!(k, "volup" | "voldown")
        && !(video_playing && matches!(k, "left" | "right"))
        && !(menu_on && matches!(k, "up" | "down"))
    {
        return;
    }

    let power_busy = shared.power.lock().unwrap().busy;
    let q_stopped = shared.st.lock().unwrap().state == "stopped";
    let vid_busy = {
        let v = shared.video.lock().unwrap();
        v.busy || v.child.is_some()
    };

    if k == "menu" && !hold && !power_busy && !menu_on && !vid_busy && (q_stopped || quran_hidden())
    {
        if let Err(e) = airplay::airplay_start(shared) {
            shared.airplay.lock().unwrap().error = e;
            airplay::airplay_publish(shared);
            toast("AirPlay could not start");
        }
        return;
    }

    if video_playing && k != "power_red" && !power_busy && video::video_key(shared, k) {
        return;
    } else if menu_on && k != "power_red" && !power_busy && video::menu_key(shared, k) {
        return;
    } else if k == "set" && !hold && !power_busy && !video_playing && !menu_on {
        video::menu_open(shared);
        return;
    }

    if k == "power_red" {
        let mut pw = shared.power.lock().unwrap();
        if !pw.busy && unix_now() - pw.woke > 8.0 {
            pw.busy = true;
            drop(pw);
            let s2 = Arc::clone(shared);
            thread::spawn(move || standby(&s2));
        }
        return;
    }
    if power_busy {
        return;
    }
    if k == "power" {
        if Path::new(SCREEN_OFF).exists() {
            screen_off(false);
            toast("Screen on");
        } else {
            screen_off(true);
        }
        return;
    }

    if q_stopped || quran_hidden() {
        let no_digits = shared.digits.lock().unwrap().buf.is_empty();
        if k == "ok" && no_digits && !vid_busy {
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "toggle".to_string());
            let s = shared.cmd(&q);
            toast(if s.state == "playing" { "Playing" } else { "Paused" });
            return;
        }
        if k == "green" && no_digits && !vid_busy {
            if shared.st.lock().unwrap().state != "playing" {
                let mut q = BTreeMap::new();
                q.insert("act".to_string(), "resume".to_string());
                shared.cmd(&q);
            }
            quran_hide(false);
            toast("Quran");
            return;
        }
        if !matches!(k, "volup" | "voldown" | "mute") {
            return;
        }
    }

    if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() {
        let (gen, buf) = {
            let mut d = shared.digits.lock().unwrap();
            d.buf.push_str(k);
            if d.buf.len() > 3 {
                let start = d.buf.len() - 3;
                d.buf = d.buf[start..].to_string();
            }
            d.gen += 1;
            (d.gen, d.buf.clone())
        };
        toast(&format!("Surah {buf}_"));
        let s2 = Arc::clone(shared);
        thread::spawn(move || digits_done(&s2, gen, 2000));
        return;
    }

    if k == "del" {
        let (gen, buf) = {
            let mut d = shared.digits.lock().unwrap();
            d.buf.pop();
            d.gen += 1;
            (d.gen, d.buf.clone())
        };
        toast(&if buf.is_empty() {
            "Cancelled".to_string()
        } else {
            format!("Surah {buf}_")
        });
        if !buf.is_empty() {
            let s2 = Arc::clone(shared);
            thread::spawn(move || digits_done(&s2, gen, 2000));
        }
        return;
    }

    if k == "ok" && !shared.digits.lock().unwrap().buf.is_empty() {
        let gen = {
            let mut d = shared.digits.lock().unwrap();
            d.gen += 1;
            d.gen
        };
        digits_done(shared, gen, 0);
        return;
    }

    match k {
        "volup" | "voldown" => {
            let cur_v = shared.st.lock().unwrap().volume;
            let nv = (cur_v + if k == "volup" { 5 } else { -5 }).clamp(0, 99);
            *shared.muted_vol.lock().unwrap() = None;
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "volume".to_string());
            q.insert("v".to_string(), nv.to_string());
            shared.cmd(&q);
            toast(&format!("Volume {nv}"));
        }
        "mute" => {
            let mut mv = shared.muted_vol.lock().unwrap();
            if let Some(prev) = mv.take() {
                drop(mv);
                shared.set_volume(prev);
                toast(&format!("Volume {}", shared.st.lock().unwrap().volume));
            } else {
                let cur = shared.st.lock().unwrap().volume;
                *mv = Some(cur);
                drop(mv);
                shared.set_volume(0);
                toast("Muted");
            }
        }
        "ok" | "green" => {
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "toggle".to_string());
            let s = shared.cmd(&q);
            toast(if s.state == "playing" { "Playing" } else { "Paused" });
        }
        "right" => {
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "nextverse".to_string());
            shared.cmd(&q);
            toast("Next verse");
        }
        "left" => {
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "prevverse".to_string());
            shared.cmd(&q);
            toast("Previous verse");
        }
        "down" => {
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "next".to_string());
            let s = shared.cmd(&q);
            toast(&format!("{}. {}", s.surah, s.name));
        }
        "up" => {
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "prev".to_string());
            let s = shared.cmd(&q);
            toast(&format!("{}. {}", s.surah, s.name));
        }
        "back" | "home" => {
            quran_hide(true);
            let playing = shared.st.lock().unwrap().state == "playing";
            toast(if playing { "Quran keeps playing" } else { "Main page" });
        }
        "menu" => {
            let cur_rep = shared.st.lock().unwrap().repeat.clone();
            let mut q = BTreeMap::new();
            q.insert("act".to_string(), "repeat".to_string());
            q.insert(
                "v".to_string(),
                if cur_rep == "surah" { "next" } else { "surah" }.to_string(),
            );
            let s = shared.cmd(&q);
            toast(if s.repeat == "surah" {
                "Repeat this surah"
            } else {
                "Then play the next surah"
            });
        }
        _ => {}
    }
}

pub fn remote_reader(shared: Arc<Shared>) {
    let mut gate = GateState::default();
    loop {
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/hi_ir")
        else {
            thread::sleep(Duration::from_secs(30));
            continue;
        };
        let fd = file.as_raw_fd();
        let mut buf = [0u8; 56 * 8];
        loop {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe { libc::poll(&mut pfd, 1, 5000) };
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            let n = n as usize;
            let mut off = 0;
            while off + 56 <= n {
                let rec = &buf[off..off + 56];
                off += 56;
                if rec[48] == 2 {
                    continue;
                }
                let code = format!(
                    "{:02x}{:02x}{:02x}{:02x}",
                    rec[8], rec[9], rec[10], rec[11]
                );
                let k = map_key(&code);
                if rec[48] == 0 {
                    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
                    let now = unsafe { libc::time(std::ptr::null_mut()) };
                    unsafe { libc::localtime_r(&now, &mut tm) };
                    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(IR_LOG) {
                        let _ = writeln!(
                            f,
                            "{:02}:{:02}:{:02} {} {}",
                            tm.tm_hour,
                            tm.tm_min,
                            tm.tm_sec,
                            code,
                            k.unwrap_or("?")
                        );
                    }
                }
                if let Some(key_name) = k {
                    if key_gate(&mut gate, key_name, rec[48] == 1) {
                        remote_key(&shared, key_name, rec[48] == 1);
                    }
                }
            }
        }
    }
}
