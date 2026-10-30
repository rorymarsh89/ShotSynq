//! FFprobe/FFmpeg access. Port of ingestor.py.

use crate::engine;
use crate::model::{Clip, Features};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

pub const SYNC_SAMPLE_RATE: u32 = 16000;
const WAVEFORM_BARS: usize = 300;

/// Locate ffmpeg/ffprobe: $SYNCUP_FFMPEG_DIR, then beside the app, then PATH.
pub fn tool_path(name: &str) -> PathBuf {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    if let Ok(dir) = std::env::var("SYNCUP_FFMPEG_DIR") {
        let p = Path::new(&dir).join(&exe);
        if p.exists() {
            return p;
        }
    }
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let p = dir.join(&exe);
            if p.exists() {
                return p;
            }
        }
    }
    PathBuf::from(exe)
}

fn command(name: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(tool_path(name));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub fn check_tools() -> Result<(), String> {
    for t in ["ffmpeg", "ffprobe"] {
        let ok = command(t).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status();
        if !matches!(ok, Ok(s) if s.success()) {
            return Err(format!("{t} not found. It should sit beside ShotSynq.exe or be on PATH."));
        }
    }
    Ok(())
}

// ── Probe ────────────────────────────────────────────────────────────────────

pub fn probe(path: &str) -> Result<Value, String> {
    let out = command("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_streams", "-show_format", path])
        .output()
        .map_err(|e| format!("Could not run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("FFprobe could not read file: {path}"));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("Bad ffprobe output: {e}"))
}

fn streams(info: &Value) -> impl Iterator<Item = &Value> {
    info["streams"].as_array().into_iter().flatten()
}

fn kind<'a>(info: &'a Value, k: &'a str) -> impl Iterator<Item = &'a Value> {
    streams(info).filter(move |s| s["codec_type"] == k)
}

fn video(info: &Value) -> Option<&Value> {
    kind(info, "video").next()
}

fn frame_rate(info: &Value) -> Option<f64> {
    let fr = video(info)?["r_frame_rate"].as_str().unwrap_or("25/1");
    let (n, d) = fr.split_once('/')?;
    let v = n.parse::<f64>().ok()? / d.parse::<f64>().ok()?;
    Some((v * 1000.0).round() / 1000.0)
}

fn audio_channels(info: &Value) -> u32 {
    let total: u64 = kind(info, "audio").map(|s| s["channels"].as_u64().unwrap_or(1)).sum();
    total.max(1) as u32
}

fn tag<'a>(tags: &'a Value, key: &str) -> Option<&'a str> {
    tags.get(key)
        .or_else(|| tags.get(key.to_uppercase()))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn timecode(info: &Value) -> String {
    if let Some(t) = tag(&info["format"]["tags"], "timecode") {
        return t.to_string();
    }
    for s in streams(info) {
        if let Some(t) = tag(&s["tags"], "timecode") {
            return t.to_string();
        }
    }
    "00:00:00:00".into()
}

fn creation_time(info: &Value) -> Option<String> {
    let ft = &info["format"]["tags"];
    for k in ["creation_time", "com.apple.quicktime.creationdate", "date"] {
        if let Some(v) = tag(ft, k) {
            return Some(v.to_string());
        }
    }
    for s in streams(info) {
        for k in ["creation_time", "date"] {
            if let Some(v) = tag(&s["tags"], k) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn fingerprint(info: &Value) -> String {
    let ch = audio_channels(info);
    if let Some(v) = video(info) {
        let codec = v["codec_name"].as_str().unwrap_or("unknown");
        let w = v["width"].as_u64().unwrap_or(1920);
        let h = v["height"].as_u64().unwrap_or(1080);
        let fps = frame_rate(info).unwrap_or(0.0);
        format!("video_{codec}_{w}x{h}_{}fps_{ch}ch", crate::model::py_float(fps))
    } else {
        let sr = kind(info, "audio").next().and_then(|s| s["sample_rate"].as_str()).unwrap_or("0");
        format!("audio_{sr}hz_{ch}ch")
    }
}

fn title_case(s: &str) -> String {
    let s = s.trim();
    if s.chars().any(|c| c.is_lowercase()) {
        return s.to_string();
    }
    // "CANON" -> "Canon"; keep short all-caps brands (DJI, JVC) as they are
    if s.len() <= 3 {
        return s.to_string();
    }
    let mut c = s.chars();
    c.next().map(|f| f.to_string() + &c.as_str().to_lowercase()).unwrap_or_default()
}

/// Best-effort device name from container metadata, then filename conventions.
fn device_name(info: &Value, path: &str) -> Option<String> {
    let ft = &info["format"]["tags"];
    let pairs = [("company_name", "product_name"), ("make", "model"), ("com.apple.quicktime.make", "com.apple.quicktime.model")];
    for (mk, md) in pairs {
        let make = tag(ft, mk).map(title_case);
        let model = tag(ft, md).map(|s| s.trim().to_string());
        match (make, model) {
            (Some(a), Some(b)) if b.to_lowercase().starts_with(&a.to_lowercase()) => return Some(b),
            (Some(a), Some(b)) => return Some(format!("{a} {b}")),
            (None, Some(b)) => return Some(b),
            _ => {}
        }
    }
    if let Some(o) = tag(ft, "originator") {
        // Broadcast WAV recorders write their model here
        return Some(o.trim().to_string());
    }
    if let Some(b) = tag(ft, "compatible_brands") {
        if b.contains("CAEP") {
            return Some("Canon".into());
        }
    }
    let name = Path::new(path).file_name()?.to_string_lossy().to_uppercase();
    for (prefix, brand) in [("TASCAM", "TASCAM"), ("ZOOM", "Zoom"), ("DJI", "DJI"), ("GOPR", "GoPro"), ("GX0", "GoPro"), ("GH0", "GoPro")] {
        if name.starts_with(prefix) {
            return Some(brand.into());
        }
    }
    None
}

pub fn load_metadata(path: &str) -> Result<Clip, String> {
    if !Path::new(path).exists() {
        return Err(format!("File not found: {path}"));
    }
    let info = probe(path)?;
    let v = video(&info);
    let duration = info["format"]["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    Ok(Clip {
        path: path.to_string(),
        name: Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        duration,
        frame_rate: frame_rate(&info),
        timecode: timecode(&info),
        creation_time: creation_time(&info),
        width: v.map(|v| v["width"].as_u64().unwrap_or(1920) as u32).unwrap_or(0),
        height: v.map(|v| v["height"].as_u64().unwrap_or(1080) as u32).unwrap_or(0),
        is_video: v.is_some(),
        num_audio_channels: audio_channels(&info),
        fingerprint: fingerprint(&info),
        device: device_name(&info, path),
        sample_rate: kind(&info, "audio").next().and_then(|s| s["sample_rate"].as_str()).and_then(|s| s.parse().ok()).unwrap_or(0),
        audio_loaded: false,
        load_error: None,
        waveform: vec![],
        features: None,
        offset: 0.0,
        synced: false,
        confidence: 0.0,
    })
}

// ── Audio ────────────────────────────────────────────────────────────────────

/// Decode one audio stream to mono f32 at `sr` (via 16-bit PCM, as the
/// Python version did through its temp WAV).
pub fn decode_audio(path: &str, stream: usize, sr: u32, limit_s: Option<f64>) -> Result<Vec<f32>, String> {
    let mut c = command("ffmpeg");
    c.args(["-v", "error", "-nostdin", "-i", path, "-map", &format!("0:a:{stream}"), "-ac", "1", "-ar", &sr.to_string(), "-vn"]);
    if let Some(t) = limit_s {
        c.args(["-t", &t.to_string()]);
    }
    c.args(["-f", "s16le", "-acodec", "pcm_s16le", "-"]);
    let mut child = c
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run ffmpeg: {e}"))?;
    let mut bytes = vec![];
    child.stdout.take().unwrap().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("ffmpeg failed: {}", err.trim()));
    }
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}

fn quality(audio: &[f32], sr: u32) -> f64 {
    if audio.is_empty() {
        return 0.0;
    }
    let rms = (audio.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / audio.len() as f64).sqrt();
    let chunk = (sr as usize * 2).max(1);
    if audio.len() <= chunk {
        return rms;
    }
    let n_chunks = (audio.len() - chunk).div_ceil(chunk);
    if n_chunks < 2 {
        return rms;
    }
    let crms: Vec<f64> = (0..n_chunks)
        .map(|k| {
            let c = &audio[k * chunk..k * chunk + chunk];
            (c.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt()
        })
        .collect();
    let m = crms.iter().sum::<f64>() / crms.len() as f64;
    let dynamic = (crms.iter().map(|v| (v - m).powi(2)).sum::<f64>() / crms.len() as f64).sqrt();
    let peaks = audio.iter().filter(|&&v| (v as f64).abs() > rms * 3.0).count() as f64 / audio.len() as f64;
    rms * 0.3 + dynamic * 0.5 + peaks * 0.2
}

/// For files with several audio streams (e.g. 4 mono tracks in an MXF), pick
/// the one with the most useful sound, judged on its first 60 s.
fn best_stream(path: &str, n: usize, sr: u32) -> usize {
    if n <= 1 {
        return 0;
    }
    let scores: Vec<f64> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..n)
            .map(|i| s.spawn(move || decode_audio(path, i, sr, Some(60.0)).map(|a| quality(&a, sr)).unwrap_or(0.0)))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or(0.0)).collect()
    });
    let mut best = 0;
    for (i, &s) in scores.iter().enumerate() {
        if s > scores[best] {
            best = i;
        }
    }
    best
}

pub fn waveform_peaks(audio: &[f32], bars: usize) -> Vec<f32> {
    if audio.is_empty() {
        return vec![0.0; bars];
    }
    let chunk = (audio.len() / bars).max(1);
    let peaks: Vec<f32> = (0..bars)
        .map(|i| {
            let s = (i * chunk).min(audio.len());
            let e = (s + chunk).min(audio.len());
            audio[s..e].iter().fold(0.0f32, |m, v| m.max(v.abs()))
        })
        .collect();
    let mx = peaks.iter().cloned().fold(0.0f32, f32::max);
    let mx = if mx > 0.0 { mx } else { 1.0 };
    peaks.iter().map(|p| p / mx).collect()
}

/// Decode audio, build the waveform and sync features. Audio isn't kept.
pub fn load_audio(clip: &mut Clip) -> Result<(), String> {
    let info = probe(&clip.path)?;
    let n_streams = kind(&info, "audio").count();
    if n_streams == 0 {
        clip.audio_loaded = true; // nothing to load; will show as unsynced
        return Ok(());
    }
    let idx = best_stream(&clip.path, n_streams, SYNC_SAMPLE_RATE);
    let audio = decode_audio(&clip.path, idx, SYNC_SAMPLE_RATE, None)?;
    clip.waveform = waveform_peaks(&audio, WAVEFORM_BARS);
    clip.features = engine::compute_features(&audio, SYNC_SAMPLE_RATE).map(Arc::new);
    clip.audio_loaded = true;
    Ok(())
}

/// Features from an already-decoded buffer (tests/CLI).
pub fn features_for(audio: &[f32]) -> Option<Features> {
    engine::compute_features(audio, SYNC_SAMPLE_RATE)
}
