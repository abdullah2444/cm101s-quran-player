use chrono::{Local, NaiveDateTime};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod draw;
mod fb;
mod prayer;
mod sys;

use draw::{Canvas, Fonts, Rgba};
use fb::{Framebuffer, H, W};
use prayer::PrayerTracker;
use sys::Stats;

pub const BG: Rgba = Rgba::rgb(15, 18, 24);
pub const CARD: Rgba = Rgba::rgb(26, 31, 41);
pub const LINE: Rgba = Rgba::rgb(42, 48, 59);
pub const TEXT: Rgba = Rgba::rgb(230, 233, 239);
pub const MUTED: Rgba = Rgba::rgb(138, 147, 163);
pub const ACCENT: Rgba = Rgba::rgb(79, 140, 255);
pub const GREEN: Rgba = Rgba::rgb(47, 179, 107);
pub const WARN: Rgba = Rgba::rgb(224, 160, 48);
pub const RED: Rgba = Rgba::rgb(224, 80, 80);

const INK: Rgba = Rgba::rgb(236, 231, 218);
const HL: Rgba = Rgba::rgb(44, 70, 118);

const QBASE: &str = "/home/ubuntu/quran";
const QNOW: &str = "/run/quran-now.json";
const VNOW: &str = "/run/video.json";
const VMENU: &str = "/run/video-menu.json";
const INTERVAL: f64 = 5.0;
const LEAD_MS: i64 = 700;
const BH: usize = 112;
const VH: usize = H - BH;

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn on_sigterm(_sig: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

#[derive(Deserialize)]
struct TimingEntry {
    v: Vec<[i64; 3]>,
}

#[derive(Deserialize)]
struct QuranMeta {
    page_size: [usize; 2],
    verse_page: HashMap<String, usize>,
    boxes: HashMap<String, HashMap<String, Vec<[i32; 4]>>>,
    timings: HashMap<String, TimingEntry>,
}

#[derive(Clone)]
struct QuranNow {
    surah: u32,
    pos_ms: i64,
    state: String,
    name: String,
    meaning: String,
    verses: u32,
    length_ms: i64,
}

#[derive(Clone, PartialEq, Eq)]
struct QuranView {
    verse: u32,
    key: String,
    page: usize,
    oy: i32,
}

#[derive(Clone)]
struct PageGrid {
    centres: Vec<f64>,
    pitch: f64,
    left: i32,
    right: i32,
}

struct MaskCache {
    items: Vec<(usize, (usize, usize, Vec<u8>))>,
}

impl MaskCache {
    fn new() -> Self {
        Self { items: Vec::new() }
    }
    fn get(&mut self, page: usize) -> Option<&(usize, usize, Vec<u8>)> {
        if let Some(idx) = self.items.iter().position(|(p, _)| *p == page) {
            let entry = self.items.remove(idx);
            self.items.push(entry);
            return self.items.last().map(|(_, d)| d);
        }
        let path = format!("{QBASE}/pages/{page:03}.png");
        let loaded = draw::load_png_luma(&path)?;
        if self.items.len() >= 4 {
            self.items.remove(0);
        }
        self.items.push((page, loaded));
        self.items.last().map(|(_, d)| d)
    }
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn quran_now() -> Option<QuranNow> {
    let raw = fs::read_to_string(QNOW).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let state = v.get("state")?.as_str()?.to_string();
    if state != "playing" && state != "paused" {
        return None;
    }
    let t = v.get("t")?.as_f64()?;
    let now = unix_now();
    if now - t >= 5.0 {
        return None;
    }
    let mut pos_ms = v.get("pos_ms").and_then(|x| x.as_i64()).unwrap_or(0);
    if state == "playing" {
        pos_ms += ((now - t) * 1000.0) as i64 + LEAD_MS;
    }
    Some(QuranNow {
        surah: v.get("surah").and_then(|x| x.as_u64()).unwrap_or(1) as u32,
        pos_ms,
        state,
        name: v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        meaning: v.get("meaning").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        verses: v.get("verses").and_then(|x| x.as_u64()).unwrap_or(1) as u32,
        length_ms: v.get("length_ms").and_then(|x| x.as_i64()).unwrap_or(1),
    })
}

fn current_verse(tim: &HashMap<u32, Vec<[i64; 3]>>, surah: u32, ms: i64) -> (u32, i64, i64) {
    let mut cur = (1, 0, 1);
    if let Some(list) = tim.get(&surah) {
        for item in list {
            if item[1] <= ms {
                cur = (item[0] as u32, item[1], item[2]);
            } else {
                break;
            }
        }
    }
    cur
}

fn quran_view(meta: &QuranMeta, tim: &HashMap<u32, Vec<[i64; 3]>>, q: &QuranNow) -> QuranView {
    let (verse, a, b) = current_verse(tim, q.surah, q.pos_ms);
    let key = format!("{}:{verse}", q.surah);
    let frac = ((q.pos_ms - a) as f64 / (b - a).max(1) as f64).clamp(0.0, 1.0);
    let first = *meta.verse_page.get(&key).unwrap_or(&1);
    let mut parts = Vec::new();
    for p in [first, first + 1] {
        if let Some(rects) = meta.boxes.get(&p.to_string()).and_then(|m| m.get(&key)) {
            if !rects.is_empty() {
                parts.push((p, rects));
            }
        }
    }
    if parts.is_empty() {
        return QuranView {
            verse,
            key,
            page: first,
            oy: 0,
        };
    }
    let nlines: usize = parts.iter().map(|(_, r)| r.len()).sum();
    let mut page = parts[0].0;
    let mut rects = parts[0].1;
    let done = parts[0].1.len() as f64 / nlines as f64;
    let mut sub = if done > 0.0 { frac / done } else { 0.0 };
    if parts.len() > 1 && frac >= done {
        page = parts[1].0;
        rects = parts[1].1;
        sub = (frac - done) / (1.0 - done);
    }
    let ph = meta.page_size[1] as f64;
    let y0 = rects.iter().map(|r| r[1]).min().unwrap() as f64 - 30.0;
    let y1 = rects.iter().map(|r| r[3]).max().unwrap() as f64 + 30.0;
    let mut oy = if y1 - y0 <= VH as f64 {
        (y0 + y1) / 2.0 - VH as f64 / 2.0
    } else {
        y0 + sub * (y1 - y0 - VH as f64)
    };
    oy = oy.clamp(0.0, ph - VH as f64);
    let mut oy_i = oy as i32;
    oy_i -= oy_i % 34;
    QuranView {
        verse,
        key,
        page,
        oy: oy_i,
    }
}

fn page_grid(meta: &QuranMeta, cache: &mut HashMap<usize, PageGrid>, page: usize) -> PageGrid {
    if let Some(g) = cache.get(&page) {
        return g.clone();
    }
    let mut rects = Vec::new();
    if let Some(m) = meta.boxes.get(&page.to_string()) {
        for v in m.values() {
            rects.extend_from_slice(v);
        }
    }
    let mut mids: Vec<f64> = rects.iter().map(|r| (r[1] + r[3]) as f64 / 2.0).collect();
    mids.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for m in mids {
        if let Some(last) = clusters.last_mut() {
            if m - *last.last().unwrap() < 50.0 {
                last.push(m);
                continue;
            }
        }
        clusters.push(vec![m]);
    }
    let centres: Vec<f64> = clusters
        .into_iter()
        .map(|g| g.iter().sum::<f64>() / g.len() as f64)
        .collect();
    let pitch = if centres.len() > 1 {
        (centres.last().unwrap() - centres[0]) / (centres.len() - 1) as f64
    } else {
        130.0
    };
    let left = rects.iter().map(|r| r[0]).min().unwrap_or(0) - 14;
    let right = rects.iter().map(|r| r[2]).max().unwrap_or(0) + 14;
    let g = PageGrid {
        centres,
        pitch,
        left,
        right,
    };
    if cache.len() > 8 {
        cache.clear();
    }
    cache.insert(page, g.clone());
    g
}

fn verse_bands(
    meta: &QuranMeta,
    grids: &mut HashMap<usize, PageGrid>,
    page: usize,
    key: &str,
) -> Vec<[i32; 4]> {
    let mut rects = meta
        .boxes
        .get(&page.to_string())
        .and_then(|m| m.get(key))
        .cloned()
        .unwrap_or_default();
    if rects.is_empty() {
        return Vec::new();
    }
    rects.sort_by_key(|r| r[1]);
    let g = page_grid(meta, grids, page);
    let cs: Vec<f64> = rects
        .iter()
        .map(|r| {
            let mid = (r[1] + r[3]) as f64 / 2.0;
            *g.centres
                .iter()
                .min_by(|a, b| (mid - **a).abs().partial_cmp(&(mid - **b).abs()).unwrap())
                .unwrap_or(&mid)
        })
        .collect();
    let n = rects.len();
    let mut out = Vec::with_capacity(n);
    for (i, r) in rects.iter().enumerate() {
        let top = if i > 0 {
            (cs[i - 1] + cs[i]) / 2.0
        } else {
            cs[i] - g.pitch / 2.0
        };
        let bot = if i + 1 < n {
            (cs[i] + cs[i + 1]) / 2.0
        } else {
            cs[i] + g.pitch / 2.0
        };
        let bx0 = if i + 1 < n || r[0] - g.left < 40 {
            g.left
        } else {
            r[0] - 6
        };
        let bx1 = if i > 0 || g.right - r[2] < 40 {
            g.right
        } else {
            r[2] + 6
        };
        out.push([bx0, top.round() as i32, bx1, bot.round() as i32]);
    }
    out
}

fn bar(c: &mut Canvas, x: i32, y: i32, w: i32, frac: f64, color: Rgba, h: i32) {
    c.fill_rect(x, y, x + w, y + h, LINE);
    if frac > 0.0 {
        let fw = ((w as f64 * frac.min(1.0)) as i32).max(h);
        c.fill_rect(x, y, x + fw, y + h, color);
    }
}

fn level_color(frac: f64) -> Rgba {
    if frac < 0.7 {
        GREEN
    } else if frac < 0.9 {
        WARN
    } else {
        RED
    }
}

fn key_chip(c: &mut Canvas, f: &Fonts, x: i32, y: i32, key: &str) {
    if key == "Red" || key == "Green" {
        c.fill_circle(x + 15, y + 18, 15, if key == "Red" { RED } else { GREEN });
        c.draw_text(f, x + 42, y, key, 28.0, true, TEXT);
    } else {
        let up = key.to_ascii_uppercase();
        let w = c.measure_text(f, &up, 24.0, true).0;
        c.stroke_rect(x, y, x + w + 28, y + 36, 3, TEXT);
        c.draw_text(f, x + 14, y + 4, &up, 24.0, true, TEXT);
        c.draw_text(f, x + w + 42, y, "key", 28.0, false, MUTED);
    }
}

fn render_home(
    f: &Fonts,
    st: &mut Stats,
    pt: &mut PrayerTracker,
    icons: &mut HashMap<String, Option<Canvas>>,
    now: NaiveDateTime,
) -> Canvas {
    let mut c = Canvas::new(W, H, BG);
    let m = 80;
    prayer::time12(&mut c, f, Some(m), 50, now, 150.0, 52.0, TEXT);
    let date_str = now.format("%A, %-d %B %Y").to_string();
    c.draw_text(f, m + 6, 230, &date_str, 40.0, false, MUTED);

    let ip = sys::ip_addr();
    let up = sys::uptime_str();
    let right = [
        ("HiNAS".to_string(), 60.0f32, true, TEXT),
        (format!("CM101s-2  ·  {ip}"), 36.0f32, false, ACCENT),
        (format!("Up {up}"), 32.0f32, false, MUTED),
    ];
    let mut ry = 66;
    for (txt, sz, bold, col) in right {
        let tw = c.measure_text(f, &txt, sz, bold).0;
        c.draw_text(f, W as i32 - m - tw, ry, &txt, sz, bold, col);
        ry += sz as i32 + 26;
    }

    let top = 300;
    let ch = 214;
    let gap = 30;
    let cw = (W as i32 - 2 * m - 3 * gap) / 4;
    let xs: Vec<i32> = (0..4).map(|i| m + i * (cw + gap)).collect();
    for &x in &xs {
        c.rounded_rect(x, top, x + cw, top + ch, 18, CARD);
    }
    let pad = 32;

    let cpu = st.cpu();
    let temp = sys::pm_temp();
    c.draw_text(f, xs[0] + pad, top + 22, "CPU", 30.0, false, MUTED);
    c.draw_text(f, xs[0] + pad, top + 62, &format!("{cpu:.0}%"), 76.0, true, TEXT);
    if let Some(t) = temp {
        let tc = if t < 75 { GREEN } else if t < 90 { WARN } else { RED };
        c.draw_text(f, xs[0] + pad, top + 158, &format!("{t}°C"), 32.0, true, tc);
    }
    let freq = sys::cpu_freq_str();
    if !freq.is_empty() {
        let fw = c.measure_text(f, &freq, 28.0, false).0;
        c.draw_text(f, xs[0] + cw - pad - fw, top + 162, &freq, 28.0, false, MUTED);
    }

    let (used, tot) = sys::mem();
    c.draw_text(f, xs[1] + pad, top + 22, "Memory", 30.0, false, MUTED);
    let mfrac = used as f64 / tot.max(1) as f64;
    c.draw_text(f, xs[1] + pad, top + 62, &format!("{:.0}%", mfrac * 100.0), 76.0, true, TEXT);
    c.draw_text(f, xs[1] + pad, top + 146, &format!("{} of {} MB", used / 1024, tot / 1024), 28.0, false, MUTED);
    bar(&mut c, xs[1] + pad, top + 186, cw - 2 * pad, mfrac, level_color(mfrac), 14);

    let ds = sys::disks();
    c.draw_text(f, xs[2] + pad, top + 22, "Storage", 30.0, false, MUTED);
    if let Some((name, u, tt)) = ds.first() {
        let dfrac = *u as f64 / (*tt).max(1) as f64;
        c.draw_text(f, xs[2] + pad, top + 62, &format!("{:.0}%", dfrac * 100.0), 76.0, true, TEXT);
        let free_txt = c.fit_text(f, &format!("{} free · {name}", sys::size_str(tt.saturating_sub(*u) as f64)), 28.0, false, cw - 2 * pad);
        c.draw_text(f, xs[2] + pad, top + 146, &free_txt, 28.0, false, MUTED);
        bar(&mut c, xs[2] + pad, top + 186, cw - 2 * pad, dfrac, level_color(dfrac), 14);
    }

    let (rx, tx) = st.net();
    c.draw_text(f, xs[3] + pad, top + 22, "Network", 30.0, false, MUTED);
    c.draw_text(f, xs[3] + pad, top + 72, &format!("↓ {}", sys::rate_str(rx)), 46.0, true, TEXT);
    c.draw_text(f, xs[3] + pad, top + 138, &format!("↑ {}", sys::rate_str(tx)), 46.0, true, MUTED);

    let mut py = top + ch + gap;
    prayer::draw_prayer_strip(&mut c, f, pt, m, py, W as i32 - 2 * m, 150, now);

    py += 150 + gap;
    let ph = 300;
    let dw = 1100;
    c.rounded_rect(m, py, m + dw, py + ph, 18, CARD);
    c.draw_text(f, m + pad, py + 26, "Apps", 34.0, true, TEXT);
    let hint = "press the key on the remote";
    let hw = c.measure_text(f, hint, 26.0, false).0;
    c.draw_text(f, m + dw - pad - hw, py + 32, hint, 26.0, false, MUTED);

    let apps = [
        ("Quran", "quran", "Green", "open the Quran"),
        ("Videos", "videos", "Set", "choose a video"),
        ("AirPlay", "videos", "Menu", "iPhone screen"),
    ];
    let tg = 16;
    let tw = (dw - 2 * pad - (apps.len() as i32 - 1) * tg) / apps.len() as i32;
    let qn = quran_now();
    for (i, (name, icon, key, mut sub)) in apps.into_iter().enumerate() {
        let tx = m + pad + i as i32 * (tw + tg);
        let ty = py + 92;
        let th = ph - 92 - pad;
        c.fill_rect(tx, ty, tx + tw, ty + th, Rgba::rgb(36, 44, 58));
        let ic = icons
            .entry(icon.to_string())
            .or_insert_with(|| draw::load_png_rgba(&format!("/var/www/html/img/png/{icon}.png"), 58, 58));
        if let Some(img) = ic {
            c.paste_canvas(img, tx + 14, ty + 20);
        }
        c.draw_text(f, tx + 82, ty + 22, name, 28.0, true, TEXT);
        let mut sc = MUTED;
        let sub_owned: String;
        if name == "Quran" {
            if let Some(ref q) = qn {
                sub_owned = format!(
                    "{}{}",
                    if q.state == "playing" { "▶ " } else { "❚❚ " },
                    q.name
                );
                sub = &sub_owned;
                sc = if q.state == "playing" { GREEN } else { WARN };
            }
        }
        let fitted = c.fit_text(f, sub, 20.0, false, tw - 90);
        c.draw_text(f, tx + 82, ty + 60, &fitted, 20.0, false, sc);
        key_chip(&mut c, f, tx + 14, ty + th - 50, key);
    }

    let sx = m + dw + gap;
    let sw2 = W as i32 - m - sx;
    c.rounded_rect(sx, py, sx + sw2, py + ph, 18, CARD);
    c.draw_text(f, sx + pad, py + 26, "Running", 34.0, true, TEXT);
    let mut names = vec!["SSH".to_string(), "Web panel".to_string()];
    names.extend_from_slice(st.services());
    let col_w = (sw2 - 2 * pad) / 2;
    for (i, n) in names.iter().take(10).enumerate() {
        let cx = sx + pad + (i as i32 / 5) * col_w;
        let cy = py + 90 + (i as i32 % 5) * 40;
        c.fill_circle(cx + 7, cy + 16, 7, GREEN);
        let cleaned = if let Some(idx) = n.find(" (") {
            &n[..idx]
        } else {
            n.as_str()
        };
        let fitted = c.fit_text(f, cleaned, 27.0, false, col_w - 40);
        c.draw_text(f, cx + 28, cy, &fitted, 27.0, false, TEXT);
    }
    c
}

fn render_quran(
    f: &Fonts,
    st: &mut Stats,
    pt: &mut PrayerTracker,
    meta: &QuranMeta,
    grids: &mut HashMap<usize, PageGrid>,
    masks: &mut MaskCache,
    q: &QuranNow,
    view: &QuranView,
    now: NaiveDateTime,
) -> Canvas {
    let mut c = Canvas::new(W, H, BG);
    let pw = meta.page_size[0] as i32;
    let ph = meta.page_size[1] as i32;
    let x = 200;
    c.fill_rect(x - 20, BH as i32, x + pw + 20, H as i32, CARD);
    for [x0, y0, x1, y1] in verse_bands(meta, grids, view.page, &view.key) {
        c.fill_rect(
            x + x0,
            BH as i32 + y0 - view.oy,
            x + x1,
            BH as i32 + y1 - view.oy,
            HL,
        );
    }
    if let Some((mw, mh, mask)) = masks.get(view.page) {
        c.paste_ink_mask(mask, *mw, *mh, x, BH as i32, view.oy.max(0) as usize, VH, INK);
    }

    let sx = x + pw + 34;
    c.fill_rect(sx, BH as i32 + 40, sx + 6, H as i32 - 40, LINE);
    let span = (H - 80 - BH) as i32;
    c.fill_rect(
        sx,
        BH as i32 + 40 + (span * view.oy / ph.max(1)),
        sx + 6,
        BH as i32 + 40 + (span * (view.oy + VH as i32) / ph.max(1)),
        ACCENT,
    );

    let pw_lbl = c.measure_text(f, "Page", 26.0, false).0;
    c.draw_text(f, x - 42 - pw_lbl, H as i32 - 120, "Page", 26.0, false, MUTED);
    let pn = view.page.to_string();
    let pnw = c.measure_text(f, &pn, 44.0, true).0;
    c.draw_text(f, x - 42 - pnw, H as i32 - 86, &pn, 44.0, true, TEXT);

    let vt = format!("{}:{}", q.surah, view.verse);
    let mut vs = 44.0f32;
    while vs > 26.0 && c.measure_text(f, &vt, vs, true).0 > x - 58 {
        vs -= 2.0;
    }
    let vtw = c.measure_text(f, &vt, vs, true).0;
    c.draw_text(f, x - 42 - vtw, BH as i32 + 40, &vt, vs, true, ACCENT);

    prayer::draw_prayer_column(&mut c, f, pt, sx + 40, W as i32 - 36, BH as i32 + 40, now);

    // slim status bar
    c.fill_rect(0, 0, W as i32, BH as i32, CARD);
    let tw = prayer::time12(&mut c, f, Some(48), 18, now, 66.0, 26.0, TEXT);
    let lx = 290.max(48 + tw + 44);
    let cpu = st.cpu();
    let t = sys::pm_temp();
    let (used, tot) = sys::mem();
    let (rx, _) = st.net();
    let line1 = format!("{}  ·  up {}", sys::ip_addr(), sys::uptime_str());
    let temp_part = t.map(|v| format!("  {v}°C")).unwrap_or_default();
    let line2 = format!(
        "CPU {cpu:.0}%{temp_part}   ·   RAM {:.0}%   ·   ↓ {}",
        used as f64 / tot.max(1) as f64 * 100.0,
        sys::rate_str(rx)
    );
    c.draw_text(f, lx, 22, &line1, 28.0, false, ACCENT);
    c.draw_text(f, lx, 62, &line2, 26.0, false, MUTED);

    let title = format!("{}. {}", q.surah, q.name);
    let sub = format!(
        "{}  ·  verse {} of {}{}",
        q.meaning,
        view.verse,
        q.verses,
        if q.state == "paused" { "  ·  paused" } else { "" }
    );
    let title_w = c.measure_text(f, &title, 44.0, true).0;
    c.draw_text(f, W as i32 - 48 - title_w, 12, &title, 44.0, true, TEXT);
    let sub_w = c.measure_text(f, &sub, 26.0, false).0;
    c.draw_text(
        f,
        W as i32 - 48 - sub_w,
        68,
        &sub,
        26.0,
        false,
        if q.state == "paused" { WARN } else { MUTED },
    );

    let total = q.length_ms.max(1) as f64;
    c.fill_rect(0, BH as i32 - 4, W as i32, BH as i32, LINE);
    c.fill_rect(
        0,
        BH as i32 - 4,
        ((W as f64) * (q.pos_ms as f64 / total).clamp(0.0, 1.0)) as i32,
        BH as i32,
        ACCENT,
    );
    c
}

fn remote_toast() -> Option<String> {
    let raw = fs::read_to_string("/run/remote-toast.json").ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let t = v.get("t")?.as_f64()?;
    if unix_now() - t < 2.5 {
        v.get("text").and_then(|x| x.as_str()).map(ToString::to_string)
    } else {
        None
    }
}

fn volume_img(v: i32) -> (Canvas, i32, i32) {
    let (n, bw, gap, h) = (20i32, 30i32, 8i32, 110usize);
    let w = (48 + 60 + 32 + n * (bw + gap) - gap + 48) as usize;
    let mut c = Canvas::new(w, h, Rgba::rgb(36, 56, 92));
    c.fill_rect(0, h as i32 - 6, w as i32, h as i32, ACCENT);
    let cy = (h as i32 - 6) / 2;
    c.fill_rect(48, cy - 12, 64, cy + 12, TEXT);
    c.fill_triangle([(64, cy - 12), (88, cy - 28), (88, cy + 28)], TEXT);
    c.fill_triangle([(64, cy - 12), (88, cy + 28), (64, cy + 12)], TEXT);
    for (i, r) in [14.0f32, 26.0].into_iter().enumerate() {
        if v > i as i32 * 50 {
            c.draw_arc(88.0, cy as f32, r, -50.0, 50.0, 4, TEXT);
        }
    }
    let on = ((v.clamp(0, 99) as f64 / 99.0) * n as f64).round() as i32;
    let x0 = 48 + 60 + 32;
    for i in 0..n {
        let bh = 18 + i * 2;
        let x = x0 + i * (bw + gap);
        let top = cy + 30 - (bh as f32 * 1.4) as i32;
        c.fill_rect(
            x,
            top,
            x + bw,
            cy + 30,
            if i < on { ACCENT } else { Rgba::rgb(60, 78, 110) },
        );
    }
    (c, (W as i32 - w as i32) / 2, H as i32 - h as i32 - 70)
}

fn toast_img(f: &Fonts, text: &str) -> (Canvas, i32, i32) {
    if let Some(rest) = text.strip_prefix("Volume ") {
        if let Ok(v) = rest.trim().parse::<i32>() {
            return volume_img(v);
        }
    }
    let dummy = Canvas::new(1, 1, BG);
    let (tw, th) = dummy.measure_text(f, text, 48.0, true);
    let (w, h) = ((tw + 96).max(1) as usize, 110usize);
    let mut c = Canvas::new(w, h, Rgba::rgb(36, 56, 92));
    c.fill_rect(0, h as i32 - 6, w as i32, h as i32, ACCENT);
    c.draw_text(f, 48, (h as i32 - 6 - th) / 2 - 4, text, 48.0, true, TEXT);
    (c, (W as i32 - w as i32) / 2, H as i32 - h as i32 - 70)
}

fn clock_txt(ms: i64) -> String {
    let mut s = (ms / 1000).max(0);
    let h = s / 3600;
    s %= 3600;
    let m = s / 60;
    s %= 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn render_video_bar(f: &Fonts, v: &Value) -> (Canvas, i32, i32) {
    let (w, h) = (W - 160, 150);
    let mut c = Canvas::new(w, h, Rgba(15, 18, 24, 230));
    c.fill_rect(0, h as i32 - 6, w as i32, h as i32, ACCENT);
    let paused = v.get("paused").and_then(|x| x.as_bool()).unwrap_or(false);
    if paused {
        c.fill_rect(40, 34, 54, 82, TEXT);
        c.fill_rect(66, 34, 80, 82, TEXT);
    } else {
        c.fill_triangle([(42, 32), (82, 58), (42, 84)], TEXT);
    }
    let pos = v.get("pos").and_then(|x| x.as_i64()).unwrap_or(0);
    let dur = v.get("dur").and_then(|x| x.as_i64()).unwrap_or(0);
    let times = if dur > 0 {
        format!("{} / {}", clock_txt(pos), clock_txt(dur))
    } else {
        clock_txt(pos)
    };
    let tw = c.measure_text(f, &times, 34.0, false).0;
    c.draw_text(f, w as i32 - 40 - tw, 30, &times, 34.0, false, MUTED);
    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
    let fitted = c.fit_text(f, title, 40.0, true, w as i32 - 112 - tw - 80);
    c.draw_text(f, 112, 24, &fitted, 40.0, true, TEXT);
    bar(
        &mut c,
        112,
        100,
        w as i32 - 112 - 40,
        if dur > 0 { pos as f64 / dur as f64 } else { 0.0 },
        ACCENT,
        12,
    );
    (c, 80, H as i32 - h as i32 - 40)
}

fn render_menu(
    f: &Fonts,
    pt: &mut PrayerTracker,
    icons: &mut HashMap<String, Option<Canvas>>,
    mn: &Value,
    now: NaiveDateTime,
) -> Canvas {
    let mut c = Canvas::new(W, H, BG);
    let m = 80;
    let ic = icons
        .entry("videos".to_string())
        .or_insert_with(|| draw::load_png_rgba("/var/www/html/img/png/videos.png", 92, 92));
    if let Some(img) = ic {
        c.paste_canvas(img, m, 44);
    }
    c.draw_text(f, m + 120, 46, "Videos", 58.0, true, TEXT);
    let tw = prayer::time12(&mut c, f, None, 0, now, 56.0, 22.0, TEXT);
    prayer::time12(&mut c, f, Some(W as i32 - m - tw), 52, now, 56.0, 22.0, TEXT);
    let path_txt = c.fit_text(
        f,
        mn.get("path").and_then(|x| x.as_str()).unwrap_or("Media"),
        30.0,
        false,
        W as i32 - 2 * m - 122 - tw - 40,
    );
    c.draw_text(f, m + 122, 120, &path_txt, 30.0, false, MUTED);

    let items = mn
        .get("items")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let sel = mn.get("sel").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let (rows, rh, top) = (9usize, 86i32, 196i32);
    let start = if items.len() > rows {
        sel.saturating_sub(rows / 2).min(items.len() - rows)
    } else {
        0
    };
    for i in start..items.len().min(start + rows) {
        let it = &items[i];
        let y = top + (i - start) as i32 * rh;
        let h = rh - 12;
        let on = i == sel;
        c.fill_rect(m, y, W as i32 - m, y + h, if on { Rgba::rgb(36, 56, 92) } else { CARD });
        if on {
            c.fill_rect(m, y, m + 8, y + h, ACCENT);
        }
        let kind = it.get("type").and_then(|x| x.as_str()).unwrap_or("file");
        let cx = m + 52;
        let cy = y + h / 2;
        let col = if kind == "continue" {
            ACCENT
        } else if on {
            TEXT
        } else {
            MUTED
        };
        if kind == "dir" {
            c.fill_rect(cx - 20, cy - 12, cx + 22, cy + 16, col);
            c.fill_rect(cx - 20, cy - 18, cx - 2, cy - 10, col);
        } else {
            c.fill_triangle([(cx - 12, cy - 18), (cx + 18, cy), (cx - 12, cy + 18)], col);
        }
        let saved = it.get("saved").and_then(|x| x.as_i64()).unwrap_or(0);
        let right = if saved > 0 {
            format!("from {}", clock_txt(saved))
        } else {
            String::new()
        };
        let rw = if right.is_empty() {
            0
        } else {
            c.measure_text(f, &right, 28.0, false).0
        };
        let bold = on || kind == "continue";
        let name = it.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let fitted = c.fit_text(f, name, 36.0, bold, W as i32 - 2 * m - 100 - rw - 60);
        let nc = if kind == "continue" && !on { ACCENT } else { TEXT };
        c.draw_text(f, m + 100, cy - 22, &fitted, 36.0, bold, nc);
        if !right.is_empty() {
            c.draw_text(f, W as i32 - m - 30 - rw, cy - 16, &right, 28.0, false, WARN);
        }
    }
    if items.is_empty() {
        c.draw_text(f, m + 20, top + 30, "No videos here", 36.0, false, MUTED);
    } else if items.len() > rows {
        let pos = format!("{} / {}", sel + 1, items.len());
        let pw = c.measure_text(f, &pos, 26.0, false).0;
        c.draw_text(f, W as i32 - m - pw, H as i32 - 62, &pos, 26.0, false, MUTED);
    }
    c.draw_text(
        f,
        m,
        H as i32 - 62,
        "▲ ▼  choose      ◀ ▶  page      OK  open / play      Back  go up      Set  close",
        26.0,
        false,
        MUTED,
    );
    if let Some(active) = pt.visible_prayer(now) {
        let banner = prayer::render_prayer_banner(f, now, Some(active));
        c.paste_canvas(&banner, (W as i32 - banner.w as i32) / 2, 14);
    }
    c
}

fn airplay_waiting(f: &Fonts) -> Canvas {
    let mut c = Canvas::new(W, H, BG);
    let mut pw = String::new();
    if let Ok(raw) = fs::read_to_string("/etc/cm101s-airplay.conf") {
        for line in raw.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some("pw") {
                if let Some(p) = it.next() {
                    if it.next().is_none() {
                        pw = p.to_string();
                        break;
                    }
                }
            }
        }
    }
    for (text, y, sz, col) in [
        ("AirPlay".to_string(), H as i32 / 2 - 140, 70.0, TEXT),
        (
            "Choose CM101s AirPlay on your iPhone".to_string(),
            H as i32 / 2 - 30,
            38.0,
            MUTED,
        ),
        (format!("Password: {pw}"), H as i32 / 2 + 35, 38.0, TEXT),
        ("Press Back to stop".to_string(), H as i32 / 2 + 105, 30.0, MUTED),
    ] {
        let tw = c.measure_text(f, &text, sz, false).0;
        c.draw_text(f, (W as i32 - tw) / 2, y, &text, sz, false, col);
    }
    c
}

fn read_recent_json(path: &str, max_age_s: f64) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    if v.get("on").and_then(|x| x.as_bool()) != Some(true) {
        return None;
    }
    let t = v.get("t").and_then(|x| x.as_f64())?;
    if unix_now() - t < max_age_s {
        Some(v)
    } else {
        None
    }
}

fn main() {
    unsafe {
        libc::nice(10);
        libc::signal(libc::SIGTERM, on_sigterm as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_sigterm as *const () as libc::sighandler_t);
    }

    let fonts = Fonts::load();
    let meta: QuranMeta = serde_json::from_str(
        &fs::read_to_string(format!("{QBASE}/quran.json")).expect("load quran.json"),
    )
    .expect("parse quran.json");
    let tim: HashMap<u32, Vec<[i64; 3]>> = meta
        .timings
        .iter()
        .filter_map(|(k, v)| k.parse::<u32>().ok().map(|n| (n, v.v.clone())))
        .collect();

    let mut fb = Framebuffer::open().expect("open /dev/fb0");
    let mut stats = Stats::new();
    let mut pt = PrayerTracker::new();
    let mut masks = MaskCache::new();
    let mut grids = HashMap::new();
    let mut icons = HashMap::new();

    let shift = [(0, 0), (6, 4), (-6, 4), (-6, -4), (6, -4)];
    let started = Instant::now();
    let mut last_draw = 0.0f64;
    let mut last_key = String::new();
    let mut blank = false;

    while RUNNING.load(Ordering::Relaxed) {
        let screen_off = Path::new("/run/screen-off").exists();
        let now_dt = Local::now().naive_local();
        pt.poll_remote_mouse(now_dt, !screen_off);

        if screen_off {
            if !blank {
                fb.clear(1);
                blank = true;
                last_key.clear();
            }
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        blank = false;

        let nowt = unix_now();
        let active = pt.visible_prayer(now_dt);
        let pkey = active
            .map(|(n, _)| format!("{}:{}", n, (nowt as i64) / 5))
            .unwrap_or_default();

        if let Some(ap) = read_recent_json("/run/airplay.json", 6.0) {
            let streaming = ap.get("streaming").and_then(|x| x.as_bool()).unwrap_or(false);
            let tt = remote_toast().unwrap_or_default();
            let key = format!("ap:{streaming}:{tt}:{pkey}");
            if key != last_key {
                if streaming {
                    fb.clear(0);
                    let banner = prayer::render_prayer_banner(&fonts, now_dt, active);
                    fb.blit_rect(&banner.data, banner.w, banner.h, W as i32 - banner.w as i32 - 40, 24);
                } else {
                    let mut img = airplay_waiting(&fonts);
                    if !tt.is_empty() {
                        let (timg, tx, ty) = toast_img(&fonts, &tt);
                        img.paste_canvas(&timg, tx, ty);
                    }
                    fb.present(&img.data, img.w, img.h);
                }
                last_draw = nowt;
                last_key = key;
            }
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        if let Some(v) = read_recent_json(VNOW, 6.0) {
            let tt = remote_toast().unwrap_or_default();
            let paused = v.get("paused").and_then(|x| x.as_bool()).unwrap_or(false);
            let shown = v.get("shown").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let pos = v.get("pos").and_then(|x| x.as_i64()).unwrap_or(0);
            let show = paused || nowt - shown < 4.0 || pos < 4000;
            let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
            let key = format!(
                "v:{show}:{}:{paused}:{title}:{tt}:{pkey}",
                if show { pos / 1000 } else { 0 }
            );
            if key != last_key {
                if !last_key.starts_with("v:") {
                    fb.clear(0);
                }
                let top = H as i32 - 360;
                let mut reg = Canvas::new(W, 360, Rgba(0, 0, 0, 0));
                if show {
                    let (b, bx, by) = render_video_bar(&fonts, &v);
                    reg.paste_canvas(&b, bx, by - top);
                }
                if !tt.is_empty() {
                    let (timg, tx, mut ty) = toast_img(&fonts, &tt);
                    if show {
                        ty = H as i32 - 150 - 40 - 24 - timg.h as i32;
                    }
                    reg.paste_canvas(&timg, tx, ty - top);
                }
                fb.blit_rect(&reg.data, reg.w, reg.h, 0, top);
                let banner = prayer::render_prayer_banner(&fonts, now_dt, active);
                fb.blit_rect(&banner.data, banner.w, banner.h, W as i32 - banner.w as i32 - 40, 24);
                last_draw = nowt;
                last_key = key;
            }
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        if let Some(mn) = read_recent_json(VMENU, 180.0) {
            let tt = remote_toast().unwrap_or_default();
            let path = mn.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let sel = mn.get("sel").and_then(|x| x.as_u64()).unwrap_or(0);
            let count = mn.get("items").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
            let key = format!("m:{path}:{sel}:{count}:{tt}:{}:{pkey}", (nowt as i64) / 60);
            if key != last_key {
                let mut img = render_menu(&fonts, &mut pt, &mut icons, &mn, now_dt);
                if !tt.is_empty() {
                    let (timg, tx, ty) = toast_img(&fonts, &tt);
                    img.paste_canvas(&timg, tx, ty);
                }
                fb.present(&img.data, img.w, img.h);
                last_draw = nowt;
                last_key = key;
            }
            thread::sleep(Duration::from_millis(40));
            continue;
        }

        let q = quran_now();
        let qv = if Path::new("/run/quran-hidden").exists() {
            None
        } else {
            q
        };
        let n = (started.elapsed().as_secs() / 600) as usize;
        let (dx, dy) = shift[n % shift.len()];
        let view = qv.as_ref().map(|qn| quran_view(&meta, &tim, qn));
        let tt = remote_toast().unwrap_or_default();
        let key = if let (Some(ref qn), Some(ref vw)) = (&qv, &view) {
            format!(
                "q:{}:{}:{}:{}:{}:{}:{tt}:{dx}:{dy}:{pkey}",
                qn.surah, qn.state, vw.verse, vw.key, vw.page, vw.oy
            )
        } else {
            format!("s:{tt}:{dx}:{dy}:{pkey}")
        };

        if key != last_key || nowt - last_draw >= INTERVAL {
            let mut img = if let (Some(ref qn), Some(ref vw)) = (&qv, &view) {
                render_quran(
                    &fonts,
                    &mut stats,
                    &mut pt,
                    &meta,
                    &mut grids,
                    &mut masks,
                    qn,
                    vw,
                    now_dt,
                )
            } else {
                render_home(&fonts, &mut stats, &mut pt, &mut icons, now_dt)
            };
            if !tt.is_empty() {
                let (timg, tx, ty) = toast_img(&fonts, &tt);
                img.paste_canvas(&timg, tx, ty);
            }
            img = img.shift(dx, dy, BG);
            fb.present(&img.data, img.w, img.h);
            last_draw = nowt;
            last_key = key;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
