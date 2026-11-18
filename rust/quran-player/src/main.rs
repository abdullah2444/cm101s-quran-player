use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod airplay;
pub mod remote;
pub mod video;

pub const BASE: &str = "/home/ubuntu/quran";
pub const NOW_PATH: &str = "/run/quran-now.json";
pub const QURAN_HIDDEN: &str = "/run/quran-hidden";
pub const SCREEN_OFF: &str = "/run/screen-off";
pub const TOAST_PATH: &str = "/run/remote-toast.json";
pub const PCM_STATUS: &str = "/proc/asound/card0/pcm0p/sub0/status";
pub const STANDBY_BIN: &str = "/usr/local/bin/box-standby";
pub const STANDBY_PY: &str = "/usr/local/bin/box-standby.py";
pub const LATENCY_MS: i64 = 350;

#[derive(Clone, Deserialize)]
pub struct ChapterInfo {
    pub name: String,
    pub meaning: String,
    pub verses: u32,
}

#[derive(Deserialize)]
struct TimingFileEntry {
    v: Vec<[i64; 3]>,
}

#[derive(Deserialize)]
struct QuranJson {
    chapters: BTreeMap<String, ChapterInfo>,
    timings: BTreeMap<String, TimingFileEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PlayerPersist {
    pub surah: u32,
    pub pos_ms: i64,
    pub volume: i32,
    pub repeat: String,
    #[serde(default)]
    pub vol_db: i32,
}

#[derive(Clone, Serialize)]
pub struct PlayerStatus {
    pub surah: u32,
    pub pos_ms: i64,
    pub state: String,
    pub volume: i32,
    pub repeat: String,
    pub verse: u32,
    pub verses: u32,
    pub name: String,
    pub meaning: String,
    pub length_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<f64>,
}

pub struct Shared {
    pub chapters: BTreeMap<u32, ChapterInfo>,
    pub timings: BTreeMap<u32, Vec<[i64; 3]>>,
    pub st: Mutex<PlayerState>,
    pub vlock: Mutex<()>,
    pub video: Mutex<video::VideoState>,
    pub airplay: Mutex<airplay::AirPlayState>,
    pub menu: Mutex<video::MenuState>,
    pub power: Mutex<remote::PowerState>,
    pub mpg_stdin: Mutex<Option<ChildStdin>>,
    pub silent: Mutex<Option<Child>>,
    pub muted_vol: Mutex<Option<i32>>,
    pub digits: Mutex<remote::DigitState>,
}

pub struct PlayerState {
    pub surah: u32,
    pub pos_ms: i64,
    pub state: String, // "playing", "paused", "stopped"
    pub volume: i32,
    pub repeat: String, // "next", "surah"
    pub clock_base: i64,
    pub clock_t0: Instant,
    pub clock_started: bool,
}

impl PlayerState {
    pub fn clock_set(&mut self, ms: i64) {
        self.clock_base = ms;
        self.clock_t0 = Instant::now() + Duration::from_millis(LATENCY_MS as u64);
        self.clock_started = true;
    }

    pub fn clock_pos(&self) -> i64 {
        if self.state == "playing" && self.clock_started {
            let now = Instant::now();
            if now <= self.clock_t0 {
                self.clock_base
            } else {
                self.clock_base + now.duration_since(self.clock_t0).as_millis() as i64
            }
        } else {
            self.pos_ms
        }
    }
}

pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn atomic_write(path: &str, data: &[u8]) {
    let tmp = format!("{path}.tmp");
    if let Ok(mut f) = File::create(&tmp) {
        if f.write_all(data).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}

pub fn toast(text: &str) {
    let payload = serde_json::json!({ "text": text, "t": unix_now() });
    atomic_write(TOAST_PATH, payload.to_string().as_bytes());
}

pub fn quran_hidden() -> bool {
    Path::new(QURAN_HIDDEN).exists()
}

pub fn quran_hide(hide: bool) {
    if hide {
        let _ = File::create(QURAN_HIDDEN);
    } else {
        let _ = fs::remove_file(QURAN_HIDDEN);
    }
}

pub fn screen_off(off: bool) {
    if off {
        let _ = File::create(SCREEN_OFF);
    } else {
        let _ = fs::remove_file(SCREEN_OFF);
    }
}

pub fn pcm_is_closed() -> bool {
    match fs::read_to_string(PCM_STATUS) {
        Ok(s) => s.trim() == "closed",
        Err(_) => true,
    }
}

impl Shared {
    pub fn load() -> Arc<Self> {
        let base = std::env::var("QURAN_BASE").unwrap_or_else(|_| BASE.to_string());
        let qpath = format!("{base}/quran.json");
        let raw = fs::read_to_string(&qpath).unwrap_or_else(|e| panic!("failed to read {qpath}: {e}"));
        let q: QuranJson = serde_json::from_str(&raw).expect("invalid quran.json");
        let chapters: BTreeMap<u32, ChapterInfo> = q
            .chapters
            .into_iter()
            .filter_map(|(k, v)| k.parse::<u32>().ok().map(|n| (n, v)))
            .collect();
        let timings: BTreeMap<u32, Vec<[i64; 3]>> = q
            .timings
            .into_iter()
            .filter_map(|(k, v)| k.parse::<u32>().ok().map(|n| (n, v.v)))
            .collect();

        let mut st = PlayerState {
            surah: 1,
            pos_ms: 0,
            state: "stopped".to_string(),
            volume: 30,
            repeat: "next".to_string(),
            clock_base: 0,
            clock_t0: Instant::now(),
            clock_started: false,
        };
        let save_path = format!("{base}/player-state.json");
        if let Ok(sraw) = fs::read_to_string(&save_path) {
            if let Ok(p) = serde_json::from_str::<Value>(&sraw) {
                if let Some(v) = p.get("surah").and_then(|x| x.as_u64()) {
                    st.surah = (v as u32).clamp(1, 114);
                }
                if let Some(v) = p.get("pos_ms").and_then(|x| x.as_i64()) {
                    st.pos_ms = v.max(0);
                }
                if let Some(v) = p.get("volume").and_then(|x| x.as_i64()) {
                    let mut vol = v as i32;
                    if p.get("vol_db").is_none() && vol > 0 {
                        vol = (99.0 + 40.0 * (vol as f64 / 99.0).log10())
                            .round()
                            .clamp(1.0, 99.0) as i32;
                    }
                    st.volume = vol.clamp(0, 99);
                }
                if let Some(v) = p.get("repeat").and_then(|x| x.as_str()) {
                    st.repeat = if v == "surah" { "surah" } else { "next" }.to_string();
                }
            }
        }

        let video_save = video::load_save();
        Arc::new(Self {
            chapters,
            timings,
            st: Mutex::new(st),
            vlock: Mutex::new(()),
            video: Mutex::new(video::VideoState::new(video_save)),
            airplay: Mutex::new(airplay::AirPlayState::default()),
            menu: Mutex::new(video::MenuState::default()),
            power: Mutex::new(remote::PowerState::default()),
            mpg_stdin: Mutex::new(None),
            silent: Mutex::new(None),
            muted_vol: Mutex::new(None),
            digits: Mutex::new(remote::DigitState::default()),
        })
    }

    pub fn silence_off(&self) {
        let mut guard = self.silent.lock().unwrap();
        if let Some(mut child) = guard.take() {
            drop(guard);
            let _ = child.kill();
            let _ = child.wait();
            for _ in 0..20 {
                if pcm_is_closed() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    pub fn mpg_send(&self, cmd: &str) {
        if cmd.starts_with("LOAD") || cmd.starts_with("PAUSE") || cmd.starts_with("JUMP") {
            self.silence_off();
        }
        let mut guard = self.mpg_stdin.lock().unwrap();
        if let Some(stdin) = guard.as_mut() {
            // mpg123 -R corrupts commands if the newline arrives in a separate write() syscall.
            let line = format!("{cmd}\n");
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        }
    }

    pub fn set_volume(&self, vol: i32) {
        let v = vol.clamp(0, 99);
        let pct = if v == 0 {
            0.0
        } else {
            100.0 * 10f64.powf((v as f64 - 99.0) * 0.5 / 20.0)
        };
        self.mpg_send(&format!("VOLUME {pct:.2}"));
        video::video_send(self, &format!("v {v}"));
    }

    pub fn verse_at(&self, surah: u32, ms: i64) -> u32 {
        let Some(v) = self.timings.get(&surah) else {
            return 1;
        };
        if v.is_empty() {
            return 1;
        }
        let idx = v.partition_point(|x| x[1] <= ms);
        v[idx.saturating_sub(1)][0].max(1) as u32
    }

    pub fn verse_ms(&self, surah: u32, verse: u32) -> i64 {
        if let Some(v) = self.timings.get(&surah) {
            for entry in v {
                if entry[0] == verse as i64 {
                    return entry[1];
                }
            }
        }
        0
    }

    pub fn save(&self) {
        let st = self.st.lock().unwrap();
        let d = PlayerPersist {
            surah: st.surah,
            pos_ms: st.pos_ms,
            volume: st.volume,
            repeat: st.repeat.clone(),
            vol_db: 1,
        };
        drop(st);
        let base = std::env::var("QURAN_BASE").unwrap_or_else(|_| BASE.to_string());
        if let Ok(json) = serde_json::to_vec(&d) {
            atomic_write(&format!("{base}/player-state.json"), &json);
        }
    }

    pub fn status(&self) -> PlayerStatus {
        let st = self.st.lock().unwrap();
        let s = st.surah;
        let ch = self.chapters.get(&s);
        let len_ms = self
            .timings
            .get(&s)
            .and_then(|v| v.last().map(|x| x[2]))
            .unwrap_or(0);
        PlayerStatus {
            surah: s,
            pos_ms: st.pos_ms,
            state: st.state.clone(),
            volume: st.volume,
            repeat: st.repeat.clone(),
            verse: self.verse_at(s, st.pos_ms),
            verses: ch.map(|c| c.verses).unwrap_or(1),
            name: ch.map(|c| c.name.clone()).unwrap_or_default(),
            meaning: ch.map(|c| c.meaning.clone()).unwrap_or_default(),
            length_ms: len_ms,
            t: None,
        }
    }

    pub fn play(&self, s: u32, start_ms: i64) {
        let s = s.clamp(1, 114);
        let base = std::env::var("QURAN_BASE").unwrap_or_else(|_| BASE.to_string());
        let vol = {
            let mut st = self.st.lock().unwrap();
            st.surah = s;
            st.pos_ms = start_ms;
            st.state = "playing".to_string();
            st.clock_set(start_ms);
            let m = *self.muted_vol.lock().unwrap();
            m.map(|_| 0).unwrap_or(st.volume)
        };
        self.mpg_send(&format!("LOAD {base}/audio/{s:03}.mp3"));
        self.set_volume(vol);
        if start_ms > 1000 {
            self.mpg_send(&format!("JUMP {:.2}s", start_ms as f64 / 1000.0));
        }
        self.save();
    }

    pub fn quran_stop(&self) {
        let mut st = self.st.lock().unwrap();
        if st.state != "stopped" {
            if st.state == "playing" {
                st.pos_ms = st.clock_pos();
            }
            st.state = "stopped".to_string();
            drop(st);
            self.mpg_send("STOP");
        }
        self.save();
    }

    pub fn track_end(&self) {
        let nxt = {
            let st = self.st.lock().unwrap();
            if st.repeat == "surah" {
                st.surah
            } else if st.surah < 114 {
                st.surah + 1
            } else {
                1
            }
        };
        self.play(nxt, 0);
    }

    pub fn cmd(&self, q: &BTreeMap<String, String>) -> PlayerStatus {
        let _vg = self.vlock.lock().unwrap();
        let a = q.get("act").map(String::as_str).unwrap_or("");
        if matches!(
            a,
            "play" | "resume" | "toggle" | "next" | "prev" | "nextverse" | "prevverse"
        ) {
            airplay::airplay_stop_locked(self);
            if video::video_busy_locked(self) {
                video::video_stop_locked(self, true);
            }
        }
        if matches!(a, "play" | "resume" | "next" | "prev") {
            quran_hide(false);
        }

        if a == "toggle" {
            let cur_state = self.st.lock().unwrap().state.clone();
            let mut nq = BTreeMap::new();
            nq.insert(
                "act".to_string(),
                if cur_state == "playing" {
                    "pause".to_string()
                } else {
                    "resume".to_string()
                },
            );
            drop(_vg);
            return self.cmd(&nq);
        }

        match a {
            "play" => {
                let cur_s = self.st.lock().unwrap().surah;
                let s = q
                    .get("surah")
                    .and_then(|x| x.parse::<u32>().ok())
                    .unwrap_or(cur_s)
                    .clamp(1, 114);
                let verse = q.get("verse").and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
                let ms = if verse > 1 { self.verse_ms(s, verse) } else { 0 };
                self.play(s, ms);
            }
            "resume" => {
                let (state, surah, pos_ms) = {
                    let st = self.st.lock().unwrap();
                    (st.state.clone(), st.surah, st.pos_ms)
                };
                if state == "paused" {
                    self.mpg_send("PAUSE");
                    let mut st = self.st.lock().unwrap();
                    st.state = "playing".to_string();
                    st.clock_set(pos_ms);
                } else if state == "stopped" {
                    self.play(surah, pos_ms);
                }
            }
            "pause" => {
                let mut st = self.st.lock().unwrap();
                if st.state == "playing" {
                    st.pos_ms = st.clock_pos();
                    st.state = "paused".to_string();
                    drop(st);
                    self.mpg_send("PAUSE");
                }
            }
            "stop" => {
                let mut st = self.st.lock().unwrap();
                st.pos_ms = st.clock_pos();
                st.state = "stopped".to_string();
                drop(st);
                self.mpg_send("STOP");
            }
            "next" | "prev" => {
                let cur_s = self.st.lock().unwrap().surah as i32;
                let nxt = ((cur_s - 1 + if a == "next" { 1 } else { -1 }).rem_euclid(114) + 1) as u32;
                self.play(nxt, 0);
            }
            "nextverse" | "prevverse" => {
                let (s, pos, state, max_v) = {
                    let mut st = self.st.lock().unwrap();
                    st.pos_ms = st.clock_pos();
                    let max_v = self.chapters.get(&st.surah).map(|c| c.verses).unwrap_or(1);
                    (st.surah, st.pos_ms, st.state.clone(), max_v)
                };
                let cur_v = self.verse_at(s, pos) as i32;
                let nv = (cur_v + if a == "nextverse" { 1 } else { -1 }).clamp(1, max_v as i32) as u32;
                let ms = self.verse_ms(s, nv);
                if state == "stopped" {
                    self.play(s, ms);
                } else {
                    self.mpg_send(&format!("JUMP {:.2}s", ms as f64 / 1000.0));
                    let mut st = self.st.lock().unwrap();
                    st.pos_ms = ms;
                    st.clock_set(ms);
                }
            }
            "volume" => {
                let v = {
                    let mut st = self.st.lock().unwrap();
                    if let Some(nv) = q.get("v").and_then(|x| x.parse::<i32>().ok()) {
                        st.volume = nv.clamp(0, 99);
                    }
                    st.volume
                };
                self.set_volume(v);
            }
            "repeat" => {
                let mut st = self.st.lock().unwrap();
                st.repeat = if q.get("v").map(String::as_str) == Some("surah") {
                    "surah".to_string()
                } else {
                    "next".to_string()
                };
            }
            _ => {}
        }

        self.save();
        self.status()
    }
}

fn spawn_mpg(shared: Arc<Shared>) {
    let mut child = Command::new("mpg123")
        .args(["-R", "-o", "alsa", "-a", "plughw:0,0", "--no-gapless"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mpg123");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    *shared.mpg_stdin.lock().unwrap() = Some(stdin);
    shared.mpg_send("SILENCE");

    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim() == "@P 0" {
                let trigger = {
                    let st = shared.st.lock().unwrap();
                    st.state == "playing"
                        && st.clock_started
                        && Instant::now() > st.clock_t0 + Duration::from_secs(3)
                };
                if trigger {
                    let s2 = Arc::clone(&shared);
                    thread::spawn(move || s2.track_end());
                }
            }
        }
        let _ = child.wait();
    });
}

fn spawn_silence_keeper(shared: Arc<Shared>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(300));
        let Ok(_vg) = shared.vlock.try_lock() else {
            continue;
        };
        let st_guard = shared.st.lock().unwrap();
        let playing = st_guard.state == "playing";
        let ap_claimed = shared.airplay.lock().unwrap().audio_claimed;
        let vid_busy = {
            let v = shared.video.lock().unwrap();
            v.busy || v.child.is_some()
        };
        if playing || ap_claimed || vid_busy {
            continue;
        }
        let alive = {
            let mut guard = shared.silent.lock().unwrap();
            if let Some(ch) = guard.as_mut() {
                matches!(ch.try_wait(), Ok(None))
            } else {
                false
            }
        };
        if alive {
            continue;
        }
        if pcm_is_closed() {
            if let Ok(child) = Command::new("aplay")
                .args([
                    "-q",
                    "-D",
                    "plughw:0,0",
                    "-f",
                    "S16_LE",
                    "-r",
                    "48000",
                    "-c",
                    "2",
                    "-t",
                    "raw",
                    "/dev/zero",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                *shared.silent.lock().unwrap() = Some(child);
            }
        }
        drop(st_guard);
    });
}

fn spawn_publisher(shared: Arc<Shared>) {
    thread::spawn(move || {
        let mut n = 0u64;
        loop {
            thread::sleep(Duration::from_millis(250));
            {
                let mut st = shared.st.lock().unwrap();
                if st.state == "playing" {
                    st.pos_ms = st.clock_pos();
                }
            }
            let mut s = shared.status();
            s.t = Some(unix_now());
            if let Ok(json) = serde_json::to_vec(&s) {
                atomic_write(NOW_PATH, &json);
            }
            if airplay::airplay_active(&shared) {
                airplay::airplay_publish(&shared);
            }
            n += 1;
            if n % 40 == 0 && s.state == "playing" {
                shared.save();
            }
        }
    });
}

fn parse_query(url: &str, keep_blank: bool) -> (String, BTreeMap<String, String>) {
    let mut parts = url.splitn(2, '?');
    let path = parts.next().unwrap_or("/").to_string();
    let mut map = BTreeMap::new();
    if let Some(qs) = parts.next() {
        for pair in qs.split('&') {
            if pair.is_empty() {
                continue;
            }
            let mut kv = pair.splitn(2, '=');
            let k = url_decode(kv.next().unwrap_or(""));
            let v = url_decode(kv.next().unwrap_or(""));
            if keep_blank || !v.is_empty() {
                map.insert(k, v);
            }
        }
    }
    (path, map)
}

fn url_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        } else if b[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn main() {
    let shared = Shared::load();
    spawn_mpg(Arc::clone(&shared));
    let initial_vol = shared.st.lock().unwrap().volume;
    shared.set_volume(initial_vol);
    video::video_publish(&shared);
    airplay::airplay_publish(&shared);
    video::menu_publish(&shared);

    spawn_silence_keeper(Arc::clone(&shared));
    spawn_publisher(Arc::clone(&shared));

    let s_ir = Arc::clone(&shared);
    thread::spawn(move || remote::remote_reader(s_ir));

    let server = tiny_http::Server::http("127.0.0.1:8790").expect("bind 127.0.0.1:8790");
    for req in server.incoming_requests() {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let (path, q) = parse_query(req.url(), req.url().starts_with("/video"));
            let body: Vec<u8> = match path.as_str() {
                "/cmd" => serde_json::to_vec(&shared.cmd(&q)).unwrap_or_default(),
                "/video" => serde_json::to_vec(&video::video_api(&shared, &q)).unwrap_or_default(),
                "/airplay" => serde_json::to_vec(&airplay::airplay_api(&shared, &q)).unwrap_or_default(),
                "/chapters" => {
                    let ch: BTreeMap<String, String> = shared
                        .chapters
                        .iter()
                        .map(|(k, v)| (k.to_string(), format!("{} - {}", v.name, v.meaning)))
                        .collect();
                    serde_json::to_vec(&ch).unwrap_or_default()
                }
                _ => serde_json::to_vec(&shared.status()).unwrap_or_default(),
            };
            let hdr = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
            let _ = req.respond(tiny_http::Response::from_data(body).with_header(hdr));
        });
    }
}
