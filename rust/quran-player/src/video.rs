use crate::{atomic_write, pcm_is_closed, screen_off, toast, unix_now, Shared};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const VIDEO_NOW: &str = "/run/video.json";
const VIDEO_SAVE: &str = "/var/lib/video-player.json";
const VMENU: &str = "/run/video-menu.json";
const VPLAY_BIN: &str = "/usr/local/bin/vplay";
const VIDEO_EXT: &[&str] = &[".mp4", ".m4v", ".mkv", ".mov", ".ts"];

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct VideoPersist {
    #[serde(default)]
    pub pos: BTreeMap<String, i64>,
    #[serde(default)]
    pub last: String,
}

pub struct VideoState {
    pub child: Option<Child>,
    pub stdin: Option<ChildStdin>,
    pub busy: bool,
    pub rel: String,
    pub pos: i64,
    pub dur: i64,
    pub paused: bool,
    pub err: String,
    pub gen: u64,
    pub shown: f64,
    pub save: VideoPersist,
}

impl VideoState {
    pub fn new(save: VideoPersist) -> Self {
        Self {
            child: None,
            stdin: None,
            busy: false,
            rel: String::new(),
            pos: 0,
            dur: 0,
            paused: false,
            err: String::new(),
            gen: 0,
            shown: 0.0,
            save,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MenuItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub rel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<i64>,
}

#[derive(Default)]
pub struct MenuState {
    pub on: bool,
    pub dir: String,
    pub items: Vec<MenuItem>,
    pub sel: usize,
    pub tk: f64,
}

#[derive(Serialize)]
pub struct VideoStatus {
    pub on: bool,
    pub rel: String,
    pub title: String,
    pub folder: String,
    pub pos: i64,
    pub dur: i64,
    pub paused: bool,
    pub err: String,
    pub shown: f64,
    pub volume: i32,
    pub t: f64,
}

fn video_root() -> String {
    std::env::var("VIDEO_ROOT").unwrap_or_else(|_| "/mnt/media".to_string())
}

pub fn load_save() -> VideoPersist {
    fs::read_to_string(VIDEO_SAVE)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn video_save_persist(save: &mut VideoPersist) {
    while save.pos.len() > 200 {
        if let Some(first) = save.pos.keys().next().cloned() {
            save.pos.remove(&first);
        }
    }
    if let Ok(json) = serde_json::to_vec(save) {
        atomic_write(VIDEO_SAVE, &json);
    }
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.as_bytes();
    let mut bi = b.as_bytes();
    while !ai.is_empty() && !bi.is_empty() {
        if ai[0].is_ascii_digit() && bi[0].is_ascii_digit() {
            let ad = ai.iter().take_while(|c| c.is_ascii_digit()).count();
            let bd = bi.iter().take_while(|c| c.is_ascii_digit()).count();
            let an: u64 = std::str::from_utf8(&ai[..ad]).unwrap_or("0").parse().unwrap_or(0);
            let bn: u64 = std::str::from_utf8(&bi[..bd]).unwrap_or("0").parse().unwrap_or(0);
            match an.cmp(&bn) {
                Ordering::Equal => {
                    ai = &ai[ad..];
                    bi = &bi[bd..];
                }
                ord => return ord,
            }
        } else {
            let ac = (ai[0] as char).to_ascii_lowercase();
            let bc = (bi[0] as char).to_ascii_lowercase();
            match ac.cmp(&bc) {
                Ordering::Equal => {
                    ai = &ai[1..];
                    bi = &bi[1..];
                }
                ord => return ord,
            }
        }
    }
    ai.len().cmp(&bi.len())
}

pub fn video_title(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn video_path(rel: &str) -> Result<PathBuf, String> {
    let root = fs::canonicalize(video_root()).map_err(|e| e.to_string())?;
    let clean = rel.trim_start_matches('/');
    let joined = if clean.is_empty() {
        root.clone()
    } else {
        root.join(clean)
    };
    let canon = fs::canonicalize(&joined).map_err(|e| e.to_string())?;
    if canon != root && !canon.starts_with(&root) {
        return Err("outside the media folder".to_string());
    }
    Ok(canon)
}

#[derive(Serialize)]
struct LsOut {
    dir: String,
    dirs: Vec<String>,
    files: Vec<String>,
    saved: BTreeMap<String, i64>,
}

fn video_ls(shared: &Shared, rel: &str) -> Result<LsOut, String> {
    let root = fs::canonicalize(video_root()).map_err(|e| e.to_string())?;
    let p = video_path(rel)?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(&p).map_err(|e| e.to_string())? {
        let Ok(e) = entry else { continue };
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(['.', '@', '#']) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            dirs.push(name);
        } else {
            let lower = name.to_ascii_lowercase();
            if VIDEO_EXT.iter().any(|ext| lower.ends_with(ext)) && !name.ends_with(".part.mp4") {
                files.push(name);
            }
        }
    }
    dirs.sort_by(|a, b| natural_cmp(a, b));
    files.sort_by(|a, b| natural_cmp(a, b));
    let rel_dir = p
        .strip_prefix(&root)
        .ok()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_string();
    let v = shared.video.lock().unwrap();
    let mut saved = BTreeMap::new();
    for f in &files {
        let full = if rel_dir.is_empty() {
            f.clone()
        } else {
            format!("{rel_dir}/{f}")
        };
        if let Some(&pos) = v.save.pos.get(&full) {
            saved.insert(f.clone(), pos);
        }
    }
    Ok(LsOut {
        dir: rel_dir,
        dirs,
        files,
        saved,
    })
}

fn video_neighbour(shared: &Shared, rel: &str, step: isize) -> Option<String> {
    let p = Path::new(rel);
    let dir = p.parent().and_then(|x| x.to_str()).unwrap_or("");
    let file = p.file_name().and_then(|x| x.to_str())?;
    let ls = video_ls(shared, dir).ok()?;
    let idx = ls.files.iter().position(|f| f == file)? as isize + step;
    if idx >= 0 && (idx as usize) < ls.files.len() {
        let f = &ls.files[idx as usize];
        Some(if dir.is_empty() {
            f.clone()
        } else {
            format!("{dir}/{f}")
        })
    } else {
        None
    }
}

pub fn video_busy_locked(shared: &Shared) -> bool {
    let v = shared.video.lock().unwrap();
    v.busy || v.child.is_some()
}

pub fn video_send(shared: &Shared, cmd: &str) {
    let mut v = shared.video.lock().unwrap();
    if let Some(stdin) = v.stdin.as_mut() {
        let line = format!("{cmd}\n");
        let _ = stdin.write_all(line.as_bytes());
        let _ = stdin.flush();
    }
}

pub fn video_status(shared: &Shared) -> VideoStatus {
    let vol = shared.st.lock().unwrap().volume;
    let v = shared.video.lock().unwrap();
    let on = v.child.is_some();
    let active_rel = if on { &v.rel } else { &v.save.last };
    let folder = Path::new(active_rel)
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let pos = if on {
        v.pos
    } else {
        *v.save.pos.get(&v.save.last).unwrap_or(&0)
    };
    VideoStatus {
        on,
        rel: active_rel.clone(),
        title: video_title(active_rel),
        folder,
        pos,
        dur: v.dur,
        paused: v.paused,
        err: v.err.clone(),
        shown: v.shown,
        volume: vol,
        t: unix_now(),
    }
}

pub fn video_publish(shared: &Shared) {
    if let Ok(json) = serde_json::to_vec(&video_status(shared)) {
        atomic_write(VIDEO_NOW, &json);
    }
}

fn video_remember_locked(v: &mut VideoState) {
    if !v.rel.is_empty() {
        if v.dur > 0 && v.pos > v.dur - 15000 {
            v.save.pos.remove(&v.rel);
        } else {
            v.save.pos.insert(v.rel.clone(), (v.pos - 3000).max(0));
        }
        v.save.last = v.rel.clone();
        video_save_persist(&mut v.save);
    }
}

pub fn video_stop_locked(shared: &Shared, remember: bool) {
    let child = {
        let mut v = shared.video.lock().unwrap();
        let Some(ch) = v.child.take() else {
            return;
        };
        v.gen += 1;
        if remember {
            video_remember_locked(&mut v);
        }
        if let Some(mut stdin) = v.stdin.take() {
            let _ = writeln!(stdin, "q");
            let _ = stdin.flush();
        }
        v.busy = false;
        Some(ch)
    };
    if let Some(mut ch) = child {
        for _ in 0..50 {
            if matches!(ch.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if matches!(ch.try_wait(), Ok(None)) {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }
    video_publish(shared);
}

pub fn video_start(shared: &Arc<Shared>, rel: &str, start_ms: Option<i64>) -> Result<(), String> {
    let path = video_path(rel)?;
    if !path.is_file() {
        return Err("no such video".to_string());
    }
    menu_close(shared);
    let _vg = shared.vlock.lock().unwrap();
    crate::airplay::airplay_stop_locked(shared);
    video_stop_locked(shared, true);
    shared.video.lock().unwrap().busy = true;
    shared.quran_stop();
    screen_off(false);
    shared.silence_off();

    for _ in 0..40 {
        if pcm_is_closed() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let start = {
        let v = shared.video.lock().unwrap();
        start_ms.unwrap_or_else(|| *v.save.pos.get(rel).unwrap_or(&0))
    };
    let vol = {
        let m = *shared.muted_vol.lock().unwrap();
        m.map(|_| 0).unwrap_or_else(|| shared.st.lock().unwrap().volume)
    };

    let mut ch = Command::new(VPLAY_BIN)
        .args([
            path.to_string_lossy().as_ref(),
            &format!("{:.1}", start as f64 / 1000.0),
            &vol.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            shared.video.lock().unwrap().busy = false;
            e.to_string()
        })?;

    let stdin = ch.stdin.take();
    let stdout = ch.stdout.take().unwrap();
    let gen = {
        let mut v = shared.video.lock().unwrap();
        v.gen += 1;
        v.child = Some(ch);
        v.stdin = stdin;
        v.busy = false;
        v.rel = rel.to_string();
        v.pos = start;
        v.dur = 0;
        v.paused = false;
        v.err.clear();
        v.shown = unix_now();
        v.save.last = rel.to_string();
        video_save_persist(&mut v.save);
        v.gen
    };
    video_publish(shared);

    let s2 = Arc::clone(shared);
    thread::spawn(move || {
        let mut ended = false;
        let mut n = 0u64;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }
            let mut v = s2.video.lock().unwrap();
            if v.gen != gen {
                break;
            }
            if parts[0] == "T" && parts.len() >= 4 {
                v.pos = parts[1].parse().unwrap_or(v.pos);
                v.dur = parts[2].parse().unwrap_or(v.dur);
                v.paused = parts[3] == "1";
                drop(v);
                video_publish(&s2);
                n += 1;
                if n % 60 == 0 {
                    let mut v2 = s2.video.lock().unwrap();
                    if v2.gen == gen {
                        video_remember_locked(&mut v2);
                    }
                }
            } else if parts[0] == "E" {
                ended = true;
            } else if parts[0] == "ERR" {
                let msg = line[4..].trim().to_string();
                toast(&msg.chars().take(60).collect::<String>());
                v.err = msg;
            }
        }
        let rel_for_next = {
            let mut v = s2.video.lock().unwrap();
            if v.gen != gen {
                return;
            }
            if let Some(mut ch) = v.child.take() {
                let _ = ch.wait();
            }
            v.stdin = None;
            let rel = v.rel.clone();
            if ended {
                v.pos = v.dur;
            }
            video_remember_locked(&mut v);
            if ended { Some(rel) } else { None }
        };
        let nxt = rel_for_next.and_then(|rel| video_neighbour(&s2, &rel, 1));
        if let Some(next_rel) = nxt {
            toast(&video_title(&next_rel).chars().take(60).collect::<String>());
            let _ = video_start(&s2, &next_rel, Some(0));
            return;
        }
        video_publish(&s2);
    });
    Ok(())
}

pub fn menu_publish(shared: &Shared) {
    let mn = shared.menu.lock().unwrap();
    let mut parts = vec!["Media"];
    if !mn.dir.is_empty() {
        parts.extend(mn.dir.split('/'));
    }
    let payload = serde_json::json!({
        "on": mn.on,
        "path": parts.join(" / "),
        "items": mn.items,
        "sel": mn.sel,
        "t": unix_now(),
    });
    drop(mn);
    atomic_write(VMENU, &payload.to_string().as_bytes());
}

pub fn menu_load(shared: &Shared, d: &str, select: Option<&str>) -> bool {
    let Ok(ls) = video_ls(shared, d) else {
        toast("Cannot open media folder");
        return false;
    };
    let mut items = Vec::new();
    let last_saved = {
        let v = shared.video.lock().unwrap();
        let last = v.save.last.clone();
        let pos = *v.save.pos.get(&last).unwrap_or(&0);
        (last, pos)
    };
    let last_parent = Path::new(&last_saved.0)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if !last_saved.0.is_empty() && (ls.dir.is_empty() || last_parent == ls.dir) {
        items.push(MenuItem {
            kind: "continue".to_string(),
            name: format!("Continue: {}", video_title(&last_saved.0)),
            rel: last_saved.0.clone(),
            saved: Some(last_saved.1),
        });
    }
    let join_rel = |n: &str| {
        if ls.dir.is_empty() {
            n.to_string()
        } else {
            format!("{}/{n}", ls.dir)
        }
    };
    for dn in &ls.dirs {
        items.push(MenuItem {
            kind: "dir".to_string(),
            name: dn.clone(),
            rel: join_rel(dn),
            saved: None,
        });
    }
    for fnm in &ls.files {
        items.push(MenuItem {
            kind: "file".to_string(),
            name: video_title(fnm),
            rel: join_rel(fnm),
            saved: Some(*ls.saved.get(fnm).unwrap_or(&0)),
        });
    }
    let mut sel = 0;
    if let Some(target) = select {
        for (i, it) in items.iter().enumerate() {
            if it.kind != "continue" && it.rel == target {
                sel = i;
                break;
            }
        }
    }
    {
        let mut mn = shared.menu.lock().unwrap();
        mn.on = true;
        mn.dir = ls.dir;
        mn.items = items;
        mn.sel = sel;
        mn.tk = unix_now();
    }
    menu_publish(shared);
    true
}

pub fn menu_open(shared: &Shared) {
    shared.quran_stop();
    screen_off(false);
    let last = shared.video.lock().unwrap().save.last.clone();
    let parent = Path::new(&last)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if last.is_empty() || !menu_load(shared, parent, Some(&last)) {
        menu_load(shared, "", None);
    }
}

pub fn menu_close(shared: &Shared) {
    let mut mn = shared.menu.lock().unwrap();
    if mn.on {
        mn.on = false;
        drop(mn);
        menu_publish(shared);
    }
}

pub fn menu_key(shared: &Arc<Shared>, k: &str) -> bool {
    {
        let mut mn = shared.menu.lock().unwrap();
        mn.tk = unix_now();
        let n = mn.items.len();
        match k {
            "up" | "down" if n > 0 => {
                let d: isize = if k == "down" { 1 } else { -1 };
                mn.sel = (mn.sel as isize + d).rem_euclid(n as isize) as usize;
                drop(mn);
                menu_publish(shared);
                return true;
            }
            "left" | "right" if n > 0 => {
                let d: isize = if k == "right" { 8 } else { -8 };
                mn.sel = (mn.sel as isize + d).clamp(0, n as isize - 1) as usize;
                drop(mn);
                menu_publish(shared);
                return true;
            }
            "ok" | "green" if n > 0 => {
                let it = mn.items[mn.sel].clone();
                drop(mn);
                if it.kind == "dir" {
                    menu_load(shared, &it.rel, None);
                } else {
                    menu_close(shared);
                    toast(&video_title(&it.rel).chars().take(60).collect::<String>());
                    let s2 = Arc::clone(shared);
                    thread::spawn(move || {
                        let _ = video_start(&s2, &it.rel, None);
                    });
                }
                return true;
            }
            "back" => {
                let cur_dir = mn.dir.clone();
                drop(mn);
                if !cur_dir.is_empty() {
                    let up = Path::new(&cur_dir)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or("");
                    menu_load(shared, up, Some(&cur_dir));
                } else {
                    menu_close(shared);
                }
                return true;
            }
            "home" | "set" | "power" => {
                drop(mn);
                menu_close(shared);
                return true;
            }
            "volup" | "voldown" | "mute" => return false,
            _ => return true,
        }
    }
}

pub fn video_key(shared: &Arc<Shared>, k: &str) -> bool {
    shared.video.lock().unwrap().shown = unix_now();
    match k {
        "ok" | "green" => {
            video_send(shared, "p");
            let mut v = shared.video.lock().unwrap();
            v.paused = !v.paused;
            drop(v);
            video_publish(shared);
            true
        }
        "left" | "right" => {
            let mut v = shared.video.lock().unwrap();
            let mut t = (v.pos + if k == "right" { 10000 } else { -10000 }).max(0);
            if v.dur > 0 {
                t = t.min((v.dur - 2000).max(0));
            }
            v.pos = t;
            drop(v);
            video_send(shared, &format!("s {:.1}", t as f64 / 1000.0));
            video_publish(shared);
            true
        }
        "up" | "down" => {
            let rel = shared.video.lock().unwrap().rel.clone();
            if let Some(nxt) = video_neighbour(shared, &rel, if k == "down" { 1 } else { -1 }) {
                toast(&video_title(&nxt).chars().take(60).collect::<String>());
                let s2 = Arc::clone(shared);
                thread::spawn(move || {
                    let _ = video_start(&s2, &nxt, Some(0));
                });
            } else {
                toast(if k == "down" { "Last episode" } else { "First episode" });
            }
            true
        }
        "back" => {
            {
                let _vg = shared.vlock.lock().unwrap();
                video_stop_locked(shared, true);
            }
            menu_open(shared);
            true
        }
        "home" | "power" => {
            {
                let _vg = shared.vlock.lock().unwrap();
                video_stop_locked(shared, true);
            }
            toast("Stopped");
            true
        }
        "volup" | "voldown" | "mute" => false,
        _ => true,
    }
}

pub fn video_api(shared: &Arc<Shared>, q: &BTreeMap<String, String>) -> Value {
    let g = |k: &str| q.get(k).cloned().unwrap_or_default();
    if let Some(dir) = q.get("ls") {
        return match video_ls(shared, dir) {
            Ok(ls) => serde_json::json!({
                "dir": ls.dir,
                "dirs": ls.dirs,
                "files": ls.files,
                "saved": ls.saved,
                "video": video_status(shared),
            }),
            Err(e) => serde_json::json!({ "error": e, "video": video_status(shared) }),
        };
    }
    if let Some(play_rel) = q.get("play") {
        let play_rel = play_rel.clone();
        let start = q.get("start").and_then(|s| s.parse::<i64>().ok());
        let s2 = Arc::clone(shared);
        thread::spawn(move || {
            let _ = video_start(&s2, &play_rel, start);
        });
        thread::sleep(Duration::from_millis(200));
    }
    let a = g("act");
    match a.as_str() {
        "stop" => {
            let _vg = shared.vlock.lock().unwrap();
            video_stop_locked(shared, true);
        }
        "toggle" | "seek" | "next" | "prev" if shared.video.lock().unwrap().child.is_some() => {
            match a.as_str() {
                "toggle" => {
                    video_key(shared, "ok");
                }
                "next" => {
                    video_key(shared, "down");
                }
                "prev" => {
                    video_key(shared, "up");
                }
                "seek" => {
                    if let Ok(sec) = g("t").parse::<f64>() {
                        let sec = sec.max(0.0);
                        video_send(shared, &format!("s {sec:.3}"));
                        shared.video.lock().unwrap().pos = (sec * 1000.0) as i64;
                        video_publish(shared);
                    }
                }
                _ => {}
            }
        }
        "resume" if shared.video.lock().unwrap().child.is_none() => {
            let last = shared.video.lock().unwrap().save.last.clone();
            if !last.is_empty() {
                let s2 = Arc::clone(shared);
                thread::spawn(move || {
                    let _ = video_start(&s2, &last, None);
                });
                thread::sleep(Duration::from_millis(200));
            }
        }
        _ => {}
    }
    serde_json::json!({ "video": video_status(shared) })
}
