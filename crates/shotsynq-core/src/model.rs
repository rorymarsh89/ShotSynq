use serde::Serialize;
use std::cmp::Ordering;
use std::sync::Arc;

/// Normalised log band energy, band-major: data[band * frames + frame].
#[derive(Debug, Clone)]
pub struct Features {
    pub bands: usize,
    pub frames: usize,
    pub data: Vec<f32>,
}

impl Features {
    pub fn band(&self, b: usize) -> &[f32] {
        &self.data[b * self.frames..(b + 1) * self.frames]
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Clip {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub frame_rate: Option<f64>,
    pub timecode: String,
    pub creation_time: Option<String>,
    pub width: u32,
    pub height: u32,
    pub is_video: bool,
    /// Total audio channels across all audio streams.
    pub num_audio_channels: u32,
    /// Format signature used to group files into tracks.
    pub fingerprint: String,
    /// Camera/recorder model if the file says ("Canon EOS C70"), else a brand guess.
    pub device: Option<String>,
    pub sample_rate: u32,
    pub audio_loaded: bool,
    pub load_error: Option<String>,
    pub waveform: Vec<f32>,
    #[serde(skip)]
    pub features: Option<Arc<Features>>,
    pub offset: f64,
    pub synced: bool,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Track {
    pub display_name: String,
    pub fingerprint: String,
    pub is_video: bool,
    pub color: String,
    pub clips: Vec<Clip>,
    pub synced: bool,
    pub confidence: f64,
}

impl Track {
    pub fn new(display_name: String, fingerprint: String, is_video: bool, color: String) -> Self {
        Track { display_name, fingerprint, is_video, color, clips: vec![], synced: false, confidence: 0.0 }
    }

    /// "Canon EOS C70 · 16 clips" or, without a known model, "4K · 25 fps · 16 clips".
    pub fn meta_string(&self) -> String {
        let n = self.clips.len();
        if n == 0 {
            return String::new();
        }
        let first = &self.clips[0];
        let clip_text = format!("{n} clip{}", if n != 1 { "s" } else { "" });
        let mut parts: Vec<String> = vec![];
        if let Some(d) = self.clips.iter().find_map(|c| c.device.clone()) {
            // Known device: "Canon EOS C70 · 16 clips" is all that's needed
            return format!("{d} · {clip_text}");
        }
        if self.is_video {
            if parts.is_empty() {
                parts.push(resolution_label(first.width, first.height));
            }
            let fps = first.frame_rate.unwrap_or(25.0);
            parts.push(format!("{} fps", if fps.fract() == 0.0 { format!("{fps:.0}") } else { format!("{fps}") }));
        } else {
            if first.sample_rate > 0 {
                let k = first.sample_rate as f64 / 1000.0;
                parts.push(format!("{} kHz", if k.fract() == 0.0 { format!("{k:.0}") } else { format!("{k:.1}") }));
            }
            parts.push(if first.num_audio_channels > 1 { format!("{} ch", first.num_audio_channels) } else { "mono".into() });
        }
        parts.push(clip_text);
        parts.join(" · ")
    }
}

fn resolution_label(w: u32, h: u32) -> String {
    match (w, h) {
        (w, _) if w >= 3800 => "4K".into(),
        (w, h) if w >= 2500 || h >= 1400 => format!("{h}p"),
        (_, h) if h > 0 => format!("{h}p"),
        _ => "Video".into(),
    }
}

/// Format a float the way Python's str() does for typical values (25.0, 29.97).
pub fn py_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

pub const TRACK_COLORS: [&str; 6] = ["#4f8ef7", "#f7934f", "#4fcf8a", "#f77fb3", "#c47af7", "#4fd6f7"];

// ── Natural sort ("C70_9" before "C70_10") ──────────────────────────────────

#[derive(PartialEq, Eq)]
enum Part {
    Text(String),
    Num(String), // digits with leading zeros stripped
}

fn natural_parts(s: &str) -> Vec<Part> {
    let mut parts = vec![];
    let mut buf = String::new();
    let mut in_num = false;
    for ch in s.chars() {
        let d = ch.is_ascii_digit();
        if d != in_num && !buf.is_empty() {
            parts.push(make_part(&buf, in_num));
            buf.clear();
        }
        in_num = d;
        buf.push(ch);
    }
    if !buf.is_empty() {
        parts.push(make_part(&buf, in_num));
    }
    parts
}

fn make_part(s: &str, num: bool) -> Part {
    if num {
        let t = s.trim_start_matches('0');
        Part::Num(if t.is_empty() { "0".into() } else { t.into() })
    } else {
        Part::Text(s.to_lowercase())
    }
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (pa, pb) = (natural_parts(a), natural_parts(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let o = match (x, y) {
            (Part::Num(x), Part::Num(y)) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
            (Part::Text(x), Part::Text(y)) => x.cmp(y),
            (Part::Num(_), Part::Text(_)) => Ordering::Less,
            (Part::Text(_), Part::Num(_)) => Ordering::Greater,
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    pa.len().cmp(&pb.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural() {
        let mut v = vec!["C70_10.mxf", "C70_9.mxf", "C70_100.mxf", "a2", "A1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["A1", "a2", "C70_9.mxf", "C70_10.mxf", "C70_100.mxf"]);
    }
}
