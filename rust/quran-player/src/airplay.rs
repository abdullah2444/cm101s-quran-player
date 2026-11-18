use crate::{atomic_write, pcm_is_closed, screen_off, unix_now, Shared};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const AIRPLAY_NOW: &str = "/run/airplay.json";
const AIRPLAY_CONFIG: &str = "/etc/cm101s-airplay.conf";
const AIRPLAY_BIN: &str = "/usr/local/lib/cm101s-airplay/uxplay";
const AIRPLAY_PLUGINS: &str = "/usr/local/lib/cm101s-airplay/gstreamer";

#[derive(Default)]
pub struct AirPlayState {
    pub child: Option<Child>,
    pub starting: bool,
    pub streaming: bool,
    pub audio_claimed: bool,
    pub error: String,
    pub gen: u64,
}

#[derive(Serialize)]
pub struct AirPlayStatus {
    pub on: bool,
    pub streaming: bool,
    pub name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub error: String,
    pub t: f64,
}

pub fn airplay_password() -> String {
    if let Ok(raw) = fs::read_to_string(AIRPLAY_CONFIG) {
        for line in raw.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some("pw") {
                if let Some(pw) = it.next() {
                    if it.next().is_none() {
                        return pw.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

pub fn airplay_active(shared: &Shared) -> bool {
    let mut ap = shared.airplay.lock().unwrap();
    if let Some(ch) = ap.child.as_mut() {
        if let Ok(Some(_)) = ch.try_wait() {
            ap.child = None;
            ap.streaming = false;
            ap.audio_claimed = false;
        }
    }
    ap.starting || ap.child.is_some()
}

pub fn airplay_status(shared: &Shared) -> AirPlayStatus {
    let on = airplay_active(shared);
    let ap = shared.airplay.lock().unwrap();
    AirPlayStatus {
        on,
        streaming: ap.streaming,
        name: "CM101s AirPlay",
        password: Some(airplay_password()),
        error: ap.error.clone(),
        t: unix_now(),
    }
}

pub fn airplay_publish(shared: &Shared) {
    let mut st = airplay_status(shared);
    st.password = None; // never leak AirPlay password into /run
    if let Ok(json) = serde_json::to_vec(&st) {
        atomic_write(AIRPLAY_NOW, &json);
    }
}

pub fn airplay_stop_locked(shared: &Shared) {
    let child = {
        let mut ap = shared.airplay.lock().unwrap();
        ap.gen += 1;
        ap.starting = false;
        ap.streaming = false;
        ap.audio_claimed = false;
        ap.child.take()
    };
    if let Some(mut p) = child {
        crate::term_and_wait(&mut p, 4000);
    }
    airplay_publish(shared);
}

pub fn airplay_start(shared: &Arc<Shared>) -> Result<(), String> {
    let _vg = shared.vlock.lock().unwrap();
    if airplay_active(shared) {
        return Ok(());
    }
    if airplay_password().is_empty() || !Path::new(AIRPLAY_BIN).is_file() {
        return Err("AirPlay is not installed or configured.".to_string());
    }

    let gen = {
        let mut ap = shared.airplay.lock().unwrap();
        ap.starting = true;
        ap.streaming = false;
        ap.audio_claimed = false;
        ap.error.clear();
        ap.gen += 1;
        ap.gen
    };
    airplay_publish(shared);

    crate::video::video_stop_locked(shared, true);
    crate::video::menu_close(shared);
    shared.quran_stop();
    screen_off(false);

    let mut cmd = Command::new("/usr/bin/stdbuf");
    cmd.args([
        "-oL",
        "-eL",
        AIRPLAY_BIN,
        "-rc",
        AIRPLAY_CONFIG,
        "-n",
        "CM101s AirPlay",
        "-nh",
        "-s",
        "1280x720@30",
        "-vs",
        "fakesink",
        "-vsync",
        "no",
        "-as",
        "alsasink device=plughw:0,0",
        "-vol",
        "0.25",
    ])
    .env("CM101S_HISI_TEST", "1")
    .env("CM101S_AUDIO_CLAIM", "1")
    .env("GST_PLUGIN_PATH", AIRPLAY_PLUGINS)
    .env("GST_REGISTRY", "/var/cache/cm101s-airplay-gst-registry.bin")
    .env_remove("CM101S_AUDIO_TEST")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    match cmd.spawn() {
        Ok(mut p) => {
            let stdout = p.stdout.take().unwrap();
            {
                let mut ap = shared.airplay.lock().unwrap();
                ap.child = Some(p);
                ap.starting = false;
            }
            airplay_publish(shared);
            let s2 = Arc::clone(shared);
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let mut ap = s2.airplay.lock().unwrap();
                    if ap.gen != gen {
                        break;
                    }
                    if line.contains("Begin streaming to GStreamer video pipeline") {
                        ap.streaming = true;
                        drop(ap);
                        airplay_publish(&s2);
                    } else if line.contains("raop_rtp_mirror->running is no longer true") {
                        ap.streaming = false;
                        ap.audio_claimed = false;
                        drop(ap);
                        airplay_publish(&s2);
                    } else if line.contains("ERROR") && !line.contains("eos proc timeout") {
                        ap.error = line.trim().chars().take(180).collect();
                    }
                }
                let mut ap = s2.airplay.lock().unwrap();
                if ap.gen == gen {
                    if let Some(mut ch) = ap.child.take() {
                        let code = ch.wait().ok().and_then(|st| st.code()).unwrap_or(0);
                        if code != 0 && ap.error.is_empty() {
                            ap.error = "AirPlay receiver stopped unexpectedly.".to_string();
                        }
                    }
                    ap.starting = false;
                    ap.streaming = false;
                    ap.audio_claimed = false;
                    drop(ap);
                    airplay_publish(&s2);
                }
            });
            Ok(())
        }
        Err(e) => {
            let mut ap = shared.airplay.lock().unwrap();
            ap.starting = false;
            ap.streaming = false;
            ap.audio_claimed = false;
            drop(ap);
            airplay_publish(shared);
            Err(e.to_string())
        }
    }
}

pub fn airplay_claim_audio(shared: &Shared) -> Result<(), String> {
    let _vg = shared.vlock.lock().unwrap();
    {
        let st = shared.st.lock().unwrap();
        if !airplay_active(shared) || st.state != "stopped" {
            return Err("AirPlay cannot claim the audio device.".to_string());
        }
        shared.airplay.lock().unwrap().audio_claimed = true;
        drop(st);
        shared.silence_off();
    }
    for _ in 0..30 {
        if pcm_is_closed() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    shared.airplay.lock().unwrap().audio_claimed = false;
    Err("The audio device is still in use.".to_string())
}

pub fn airplay_api(shared: &Arc<Shared>, q: &BTreeMap<String, String>) -> serde_json::Value {
    let act = q.get("act").map(String::as_str).unwrap_or("");
    let res = match act {
        "start" => airplay_start(shared),
        "stop" => {
            let _vg = shared.vlock.lock().unwrap();
            airplay_stop_locked(shared);
            Ok(())
        }
        "claim_audio" => match airplay_claim_audio(shared) {
            Ok(()) => return serde_json::json!({ "claimed": true }),
            Err(e) => Err(e),
        },
        "" => Ok(()),
        _ => Err("Unknown AirPlay action.".to_string()),
    };
    if let Err(e) = res {
        shared.airplay.lock().unwrap().error = e;
        airplay_publish(shared);
    }
    serde_json::to_value(airplay_status(shared)).unwrap_or_default()
}
