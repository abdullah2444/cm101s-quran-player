use crate::draw::{Canvas, Fonts, Rgba};
use crate::{ACCENT, MUTED, TEXT, WARN};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const PRAYERS: [&str; 6] = ["Fajr", "Sunrise", "Dhuhr", "Asr", "Maghrib", "Isha"];
const FAJR_ANGLE: f64 = 18.0;
const ISHA_ANGLE: f64 = 17.0;
const ASR_FACTOR: f64 = 1.0;
const PRAYER_WINDOW_MINS: i64 = 20;
const PRAYER_ACK: &str = "/var/lib/prayer-reminder-ack.json";
const IR_LOG: &str = "/run/ir-keys.log";
const PRAYER_TOAST: &str = "/run/remote-toast.json";

#[derive(Deserialize)]
pub struct LocationCfg {
    pub lat: f64,
    pub lng: f64,
    pub tz: f64,
    pub city: String,
}

impl LocationCfg {
    pub fn load() -> Self {
        let mut cfg = Self {
            lat: 21.4225,
            lng: 39.8262,
            tz: 3.0,
            city: "Makkah".to_string(),
        };
        if let Ok(raw) = fs::read_to_string("/etc/prayer-location.json") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(v) = parsed.get("lat").and_then(|x| x.as_f64()) {
                    cfg.lat = v;
                }
                if let Some(v) = parsed.get("lng").and_then(|x| x.as_f64()) {
                    cfg.lng = v;
                }
                if let Some(v) = parsed.get("tz").and_then(|x| x.as_f64()) {
                    cfg.tz = v;
                }
                if let Some(v) = parsed.get("city").and_then(|x| x.as_str()) {
                    cfg.city = v.to_string();
                }
            }
        }
        cfg
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct AckRecord {
    #[serde(default)]
    prayer: String,
    #[serde(default)]
    start: String,
}

pub struct PrayerTracker {
    pub loc: LocationCfg,
    cache: HashMap<NaiveDate, Vec<(&'static str, NaiveDateTime)>>,
    ack_loaded: bool,
    ack: AckRecord,
    ir_init: bool,
    ir_id: Option<(u64, u64)>,
    ir_off: u64,
    ir_partial: String,
    ir_last: Option<Instant>,
}

impl PrayerTracker {
    pub fn new() -> Self {
        Self {
            loc: LocationCfg::load(),
            cache: HashMap::new(),
            ack_loaded: false,
            ack: AckRecord::default(),
            ir_init: false,
            ir_id: None,
            ir_off: 0,
            ir_partial: String::new(),
            ir_last: None,
        }
    }

    fn sun(jd: f64) -> (f64, f64) {
        let d = jd - 2451545.0;
        let g = (357.529 + 0.98560028 * d).rem_euclid(360.0);
        let q = (280.459 + 0.98564736 * d).rem_euclid(360.0);
        let l = (q + 1.915 * g.to_radians().sin() + 0.020 * (2.0 * g).to_radians().sin())
            .rem_euclid(360.0);
        let e = 23.439 - 0.00000036 * d;
        let ra = (e.to_radians().cos() * l.to_radians().sin())
            .atan2(l.to_radians().cos())
            .to_degrees()
            / 15.0;
        let ra = ra.rem_euclid(24.0);
        let eqt = q / 15.0 - ra;
        let decl = (e.to_radians().sin() * l.to_radians().sin()).asin().to_degrees();
        (decl, eqt)
    }

    pub fn times(&mut self, day: NaiveDate) -> Vec<(&'static str, NaiveDateTime)> {
        if let Some(v) = self.cache.get(&day) {
            return v.clone();
        }
        let mut y = day.year();
        let mut m = day.month() as i32;
        let d = day.day() as i32;
        if m <= 2 {
            y -= 1;
            m += 12;
        }
        let a = y / 100;
        let jd = (365.25 * (y + 4716) as f64).floor()
            + (30.6001 * (m + 1) as f64).floor()
            + d as f64
            + 2.0
            - a as f64
            + (a / 4) as f64
            - 1524.5
            - self.loc.lng / 360.0;

        let lat = self.loc.lat;
        let noon = |t: f64| -> f64 { (12.0 - Self::sun(jd + t).1).rem_euclid(24.0) };
        let angle_time = |angle: f64, t: f64, before: bool| -> f64 {
            let decl = Self::sun(jd + t).0;
            let c = (-angle.to_radians().sin() - decl.to_radians().sin() * lat.to_radians().sin())
                / (decl.to_radians().cos() * lat.to_radians().cos());
            let span = c.clamp(-1.0, 1.0).acos().to_degrees() / 15.0;
            noon(t) + if before { -span } else { span }
        };
        let asr = |t: f64| -> f64 {
            let decl = Self::sun(jd + t).0;
            let ang = -(1.0 / (ASR_FACTOR + (lat - decl).abs().to_radians().tan()))
                .atan()
                .to_degrees();
            angle_time(ang, t, false)
        };

        let hours = [
            angle_time(FAJR_ANGLE, 5.0 / 24.0, true),
            angle_time(0.833, 6.0 / 24.0, true),
            noon(12.0 / 24.0),
            asr(13.0 / 24.0),
            angle_time(0.833, 18.0 / 24.0, false),
            angle_time(ISHA_ANGLE, 18.0 / 24.0, false),
        ];
        let base = day.and_hms_opt(0, 0, 0).unwrap();
        let mut out = Vec::with_capacity(6);
        for (&name, hr) in PRAYERS.iter().zip(hours.iter()) {
            let adj = hr + self.loc.tz - self.loc.lng / 15.0;
            let mins = (adj * 60.0).round() as i64;
            out.push((name, base + Duration::minutes(mins)));
        }
        if self.cache.len() >= 3 {
            self.cache.clear();
        }
        self.cache.insert(day, out.clone());
        out
    }

    pub fn next_prayer(&mut self, now: NaiveDateTime) -> (&'static str, NaiveDateTime) {
        let today = now.date();
        for day in [today, today + Duration::days(1)] {
            for (name, t) in self.times(day) {
                if name != "Sunrise" && t > now {
                    return (name, t);
                }
            }
        }
        ("Fajr", now)
    }

    pub fn prayer_window(&mut self, now: NaiveDateTime) -> Option<(&'static str, NaiveDateTime)> {
        let win = Duration::minutes(PRAYER_WINDOW_MINS);
        let today = now.date();
        let prev = (now - win).date();
        let mut best: Option<(NaiveDateTime, &'static str)> = None;
        let days: Vec<NaiveDate> = if prev != today { vec![today, prev] } else { vec![today] };
        for d in days {
            for (name, t) in self.times(d) {
                if name != "Sunrise" && t <= now && now < t + win {
                    if best.is_none() || t > best.unwrap().0 {
                        best = Some((t, name));
                    }
                }
            }
        }
        best.map(|(start, name)| (name, start + win))
    }

    fn is_acknowledged(&mut self, active: (&'static str, NaiveDateTime)) -> bool {
        if !self.ack_loaded {
            self.ack_loaded = true;
            if let Ok(raw) = fs::read_to_string(PRAYER_ACK) {
                if let Ok(rec) = serde_json::from_str(&raw) {
                    self.ack = rec;
                }
            }
        }
        let win = Duration::minutes(PRAYER_WINDOW_MINS);
        let start_iso = (active.1 - win).format("%Y-%m-%dT%H:%M:%S").to_string();
        self.ack.prayer == active.0 && self.ack.start == start_iso
    }

    pub fn visible_prayer(&mut self, now: NaiveDateTime) -> Option<(&'static str, NaiveDateTime)> {
        let active = self.prayer_window(now)?;
        if self.is_acknowledged(active) {
            None
        } else {
            Some(active)
        }
    }

    fn acknowledge(&mut self, when: NaiveDateTime) {
        let Some(active) = self.prayer_window(when) else {
            return;
        };
        if self.is_acknowledged(active) {
            return;
        }
        let win = Duration::minutes(PRAYER_WINDOW_MINS);
        let rec = AckRecord {
            prayer: active.0.to_string(),
            start: (active.1 - win).format("%Y-%m-%dT%H:%M:%S").to_string(),
        };
        if let Ok(json) = serde_json::to_vec(&rec) {
            let tmp = format!("{PRAYER_ACK}.tmp");
            if let Ok(mut f) = File::create(&tmp) {
                if f.write_all(&json).is_ok() && fs::rename(&tmp, PRAYER_ACK).is_ok() {
                    self.ack = rec;
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    let toast = serde_json::json!({
                        "text": format!("{} marked as prayed", active.0),
                        "t": ts,
                    });
                    let ttmp = format!("{PRAYER_TOAST}.prayer.tmp");
                    if let Ok(mut tf) = File::create(&ttmp) {
                        if tf.write_all(toast.to_string().as_bytes()).is_ok() {
                            let _ = fs::rename(&ttmp, PRAYER_TOAST);
                        }
                    }
                }
            }
        }
    }

    pub fn poll_remote_mouse(&mut self, now: NaiveDateTime, enabled: bool) {
        let tick = Instant::now();
        if let Some(last) = self.ir_last {
            if tick.duration_since(last) < std::time::Duration::from_millis(200) {
                return;
            }
        }
        self.ir_last = Some(tick);

        let Ok(mut f) = File::open(IR_LOG) else {
            self.ir_init = true;
            return;
        };
        let Ok(meta) = f.metadata() else {
            return;
        };
        let id = (meta.dev(), meta.ino());
        let len = meta.len();
        if !self.ir_init {
            self.ir_init = true;
            self.ir_id = Some(id);
            self.ir_off = len;
            return;
        }
        if self.ir_id != Some(id) || len < self.ir_off {
            self.ir_id = Some(id);
            self.ir_off = 0;
            self.ir_partial.clear();
        }
        if f.seek(SeekFrom::Start(self.ir_off)).is_err() {
            return;
        }
        let mut data = String::new();
        if f.read_to_string(&mut data).is_err() {
            return;
        }
        self.ir_off += data.len() as u64;

        let combined = format!("{}{data}", self.ir_partial);
        let mut lines: Vec<&str> = combined.split('\n').collect();
        self.ir_partial = lines.pop().unwrap_or("").to_string();
        if !enabled {
            return;
        }
        for line in lines {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 || fields[1] != "01fe04fb" || fields[2] != "mouse" {
                continue;
            }
            let hms: Vec<u32> = fields[0].split(':').filter_map(|x| x.parse().ok()).collect();
            if hms.len() != 3 {
                continue;
            }
            let Some(nt) = NaiveTime::from_hms_opt(hms[0], hms[1], hms[2]) else {
                continue;
            };
            let mut when = now.date().and_time(nt);
            if when - now > Duration::hours(12) {
                when -= Duration::days(1);
            }
            let age = (now - when).num_seconds();
            if (-2..=15).contains(&age) {
                self.acknowledge(when);
            }
        }
    }
}

pub fn prayer_countdown(now: NaiveDateTime, end: NaiveDateTime) -> String {
    let ms = (end - now).num_milliseconds().max(0);
    let secs = (ms + 999) / 1000;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

pub fn until(now: NaiveDateTime, t: NaiveDateTime) -> String {
    let m = (t - now).num_seconds() / 60 + 1;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

pub fn time12(
    c: &mut Canvas,
    f: &Fonts,
    x: Option<i32>,
    y: i32,
    t: NaiveDateTime,
    big: f32,
    small: f32,
    color: Rgba,
) -> i32 {
    let hr = match t.hour() % 12 {
        0 => 12,
        h => h,
    };
    let hm = format!("{}:{:02}", hr, t.minute());
    let ap = if t.hour() < 12 { "AM" } else { "PM" };
    let w1 = c.measure_text(f, &hm, big, true).0;
    let w2 = c.measure_text(f, ap, small, true).0;
    let gap = (small as i32 / 4).max(4);
    if let Some(px) = x {
        c.draw_text(f, px, y, &hm, big, true, color);
        c.draw_text(
            f,
            px + w1 + gap,
            y + ((big - small) * 0.85) as i32,
            ap,
            small,
            true,
            color,
        );
    }
    w1 + gap + w2
}

pub fn mouse_hint(c: &mut Canvas, f: &Fonts, x: i32, y: i32, size: f32, color: Rgba) {
    c.draw_text(f, x, y, "Press", size, false, color);
    let pw = c.measure_text(f, "Press", size, false).0;
    let mx = x + pw + 9;
    let mw = ((size * 0.75) as i32).max(12);
    let mh = ((size * 1.05) as i32).max(18);
    c.draw_line(mx + mw / 2, y, mx + mw / 2, y + 3, 2, color);
    c.stroke_ellipse(mx, y + 3, mx + mw, y + 3 + mh, 2, color);
    c.draw_line(mx + mw / 2, y + 4, mx + mw / 2, y + 3 + mh / 2, 2, color);
    c.draw_text(f, mx + mw + 9, y, "to close", size, false, color);
}

pub fn draw_prayer_strip(
    c: &mut Canvas,
    f: &Fonts,
    pt: &mut PrayerTracker,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    now: NaiveDateTime,
) {
    c.rounded_rect(x, y, x + w, y + h, 18, crate::CARD);
    let (nxt, nt) = pt.next_prayer(now);
    let active = pt.visible_prayer(now);
    let times = pt.times(now.date());
    let title = if active.is_some() {
        "Time to pray"
    } else {
        "Prayer times"
    };
    let msg = if let Some((name, end)) = active {
        format!("{name}  {} left", prayer_countdown(now, end))
    } else {
        format!("{nxt} in {}", until(now, nt))
    };
    c.draw_text(
        f,
        x + 32,
        y + 26,
        title,
        30.0,
        false,
        if active.is_some() { WARN } else { MUTED },
    );
    let mut fs = 40.0f32;
    while fs > 30.0 && c.measure_text(f, &msg, fs, true).0 > 448 {
        fs -= 2.0;
    }
    c.draw_text(
        f,
        x + 32,
        y + 66,
        &msg,
        fs,
        true,
        if active.is_some() { WARN } else { TEXT },
    );
    if active.is_some() {
        mouse_hint(c, f, x + 32, y + 112, 22.0, MUTED);
    } else {
        c.draw_text(f, x + 32, y + 116, &pt.loc.city, 22.0, false, MUTED);
    }

    let left = 480;
    let colw = (w - left - 20) / 6;
    let win = Duration::minutes(PRAYER_WINDOW_MINS);
    for (i, (name, t)) in times.into_iter().enumerate() {
        let cx = x + left + (i as i32) * colw;
        let is_next = name == nxt && t.date() == nt.date();
        let is_active = active
            .map(|(an, end)| name == an && t == end - win)
            .unwrap_or(false);
        if is_active {
            c.fill_rect(cx + 6, y + 18, cx + colw - 6, y + h - 18, Rgba::rgb(68, 49, 20));
        } else if is_next {
            c.fill_rect(cx + 6, y + 18, cx + colw - 6, y + h - 18, Rgba::rgb(36, 56, 92));
        }
        let passed = t <= now && !is_active;
        let nc = if is_active {
            WARN
        } else if is_next {
            ACCENT
        } else {
            MUTED
        };
        let tc = if passed { Rgba::rgb(100, 108, 122) } else { TEXT };
        let nw = c.measure_text(f, name, 28.0, false).0;
        c.draw_text(f, cx + (colw - nw) / 2, y + 34, name, 28.0, false, nc);
        let tw = time12(c, f, None, 0, t, 50.0, 22.0, tc);
        time12(c, f, Some(cx + (colw - tw) / 2), y + 74, t, 50.0, 22.0, tc);
    }
}

pub fn draw_prayer_column(
    c: &mut Canvas,
    f: &Fonts,
    pt: &mut PrayerTracker,
    x0: i32,
    x1: i32,
    mut y: i32,
    now: NaiveDateTime,
) {
    let (nxt, nt) = pt.next_prayer(now);
    let active = pt.visible_prayer(now);
    let times = pt.times(now.date());
    c.draw_text(
        f,
        x0,
        y,
        if active.is_some() {
            "Time to pray"
        } else {
            "Next prayer"
        },
        30.0,
        false,
        if active.is_some() { WARN } else { MUTED },
    );
    let main_name = active.map(|(n, _)| n).unwrap_or(nxt);
    c.draw_text(f, x0, y + 40, main_name, 46.0, true, TEXT);
    let msg = if let Some((_, end)) = active {
        format!("{} left", prayer_countdown(now, end))
    } else {
        format!("in {}", until(now, nt))
    };
    c.draw_text(
        f,
        x0,
        y + 100,
        &msg,
        36.0,
        true,
        if active.is_some() { WARN } else { ACCENT },
    );
    if active.is_some() {
        mouse_hint(c, f, x0, y + 145, 22.0, MUTED);
    }
    y += 184;

    let win = Duration::minutes(PRAYER_WINDOW_MINS);
    for (name, t) in times {
        let is_next = name == nxt && t.date() == nt.date();
        let is_active = active
            .map(|(an, end)| name == an && t == end - win)
            .unwrap_or(false);
        if is_active {
            c.fill_rect(x0 - 14, y - 12, x1 + 14, y + 56, Rgba::rgb(68, 49, 20));
        } else if is_next {
            c.fill_rect(x0 - 14, y - 12, x1 + 14, y + 56, Rgba::rgb(36, 56, 92));
        }
        let passed = t <= now && !is_active;
        let col = if is_active {
            WARN
        } else if is_next {
            ACCENT
        } else if passed {
            Rgba::rgb(96, 104, 118)
        } else {
            TEXT
        };
        let tw = time12(c, f, None, 0, t, 42.0, 20.0, col);
        let bold = is_next || is_active;
        let avail = (x1 - x0 - tw - 16).max(0);
        let mut fs = 36.0f32;
        while fs > 24.0 && c.measure_text(f, name, fs, bold).0 > avail {
            fs -= 2.0;
        }
        let fitted = c.fit_text(f, name, fs, bold, avail);
        c.draw_text(f, x0, y + 2 + (36 - fs as i32) / 2, &fitted, fs, bold, col);
        time12(c, f, Some(x1 - tw), y - 2, t, 42.0, 20.0, col);
        y += 80;
    }
}

pub fn render_prayer_banner(
    f: &Fonts,
    now: NaiveDateTime,
    active: Option<(&'static str, NaiveDateTime)>,
) -> Canvas {
    let mut c = Canvas::new(460, 100, Rgba(0, 0, 0, 0));
    if let Some((name, end)) = active {
        c.fill_rect(0, 0, 460, 100, crate::CARD.with_a(235));
        c.fill_rect(0, 95, 460, 100, WARN);
        c.draw_text(f, 20, 8, &format!("{name}  ·  Time to pray"), 25.0, true, WARN);
        let label = format!("{} left", prayer_countdown(now, end));
        c.draw_text(f, 20, 40, &label, 38.0, true, TEXT);
        let lw = c.measure_text(f, &label, 38.0, true).0;
        mouse_hint(&mut c, f, 20 + lw + 16, 57, 16.0, MUTED);
    }
    c
}
