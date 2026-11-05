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

#[derive(Debug, Clone, Default, Serialize)]
pub struct Clip {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub frame_rate: Option<f64>,
    pub timecode: String,
    /// The file carries its own start timecode (timecode is "00:00:00:00" otherwise).
    pub has_timecode: bool,
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
    /// Recording start (seconds, any epoch) when the file says; used to tell
    /// identical devices apart.
    pub rec_start: Option<f64>,
    /// Why this clip can't be synced before trying, e.g. "silent audio".
    pub audio_note: Option<String>,
    /// Shared by files recorded together by one device (see `take_key`).
    pub take_key: Option<String>,
    /// Was part of the last sync (false = added afterwards).
    pub sync_tried: bool,
    /// Which drop of files this came in with.
    pub batch: u64,
    /// Why the last sync couldn't place it, when known.
    pub sync_note: Option<String>,
    /// Placed by hand.
    pub manual: bool,
    /// A weak match the user has looked at and accepted.
    pub confirmed: bool,
    /// The user said its match was wrong: left out of syncing until they
    /// place it by hand.
    pub excluded: bool,
    /// Id of the lane this clip's device was given when loaded. Sync treats
    /// clips with the same device lane as one device, wherever they're moved.
    pub device_lane: u64,
    /// For a hand-placed clip: the clip it's pinned to (path) and its start
    /// minus that clip's start, so the placement survives a re-sync.
    pub pin: Option<(String, f64)>,
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
    /// Stable id (track indices shift as tracks come and go).
    pub id: u64,
    /// Name was generated (not typed by the user), so it may be renumbered.
    pub auto_name: bool,
    /// Made by the user with "New lane": kept even when empty.
    pub user_lane: bool,
}

impl Track {
    pub fn new(display_name: String, fingerprint: String, is_video: bool, color: String) -> Self {
        Track {
            display_name,
            fingerprint,
            is_video,
            color,
            clips: vec![],
            synced: false,
            confidence: 0.0,
            id: 0,
            auto_name: true,
            user_lane: false,
        }
    }

    /// "Canon EOS C70 · 16 clips" or, without a known model, "4K · 25 fps · 16 clips".
    /// Lines shown under the lane name, e.g.
    /// ["Canon EOS C70 · 14 clips", "11m duration"].
    pub fn meta_lines(&self) -> Vec<String> {
        let n = self.clips.len();
        if n == 0 {
            return vec![];
        }
        // Device on its own line (cut short if long), counts on the next, so a
        // long camera name never wraps
        let count = format!("{n} clip{}", if n == 1 { "" } else { "s" });
        let total: f64 = self.clips.iter().map(|c| c.duration).sum();
        let stats = format!("{count} · {}", fmt_duration(total));
        match self.clips.iter().find_map(|c| c.device.clone()) {
            Some(d) => vec![d, stats],
            None => vec![stats],
        }
    }
}

/// 45 -> "45s", 660 -> "11m", 3900 -> "1h 05m"
pub fn fmt_duration(secs: f64) -> String {
    if secs < 60.0 {
        return format!("{}s", secs.round() as u64);
    }
    let m = (secs / 60.0).round() as u64;
    if m < 60 { format!("{m}m") } else { format!("{}h {:02}m", m / 60, m % 60) }
}

/// Format a float the way Python's str() does for typical values (25.0, 29.97).
pub fn py_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// Track colours. No reds, oranges, pinks or yellows: red marks unsynced
/// clips and amber marks weak matches, so those must stand out.
/// Ten, so the first ten tracks never share a colour.
pub const TRACK_COLORS: [&str; 10] = [
    "#4f8ef7", "#4fcf8a", "#b07af7", "#4fd6f7", "#a6d96a",
    "#8fa3bf", "#8b93ff", "#2fa89a", "#d0d4dc", "#d4bfff",
];

/// Files that are parts of one multi-file recording share a take key, e.g.
/// Tascam "AUDIO__0001S12.wav" + "AUDIO__0001S34D06.wav" -> "audio__0001",
/// Zoom "ZOOM0001_Tr1.WAV" + "ZOOM0001_TrLR.WAV" -> "zoom0001",
/// "Interview_safety.wav" -> "interview". None if the name has no such marker.
pub fn take_key(name: &str) -> Option<String> {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name).to_lowercase();
    // Tascam: <take>S<1-2 digits>[D<0-2 digits>]
    let b = stem.as_bytes();
    let mut i = b.len();
    let mut j = i;
    while j > 0 && b[j - 1].is_ascii_digit() && i - j < 2 {
        j -= 1;
    }
    if j > 0 && b[j - 1] == b'd' {
        i = j - 1; // dual-record suffix
    }
    let mut k = i;
    while k > 0 && b[k - 1].is_ascii_digit() && i - k < 2 {
        k -= 1;
    }
    if k < i && k > 1 && b[k - 1] == b's' && b[k - 2].is_ascii_digit() {
        return Some(stem[..k - 1].to_string());
    }
    // Zoom: <take>_Tr<...>
    if let Some(pos) = stem.rfind("_tr") {
        if pos > 0 && stem.len() > pos + 3 {
            return Some(stem[..pos].to_string());
        }
    }
    // Explicit safety/backup naming
    for tok in ["_safety", " safety", "-safety", "_backup", " backup", "-backup", "_bu", "-bu"] {
        if let Some(pos) = stem.rfind(tok) {
            if pos > 0 {
                return Some(stem[..pos].to_string());
            }
        }
    }
    None
}


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
    fn take_keys() {
        assert_eq!(take_key("AUDIO__0001S12.wav").as_deref(), Some("audio__0001"));
        assert_eq!(take_key("AUDIO__0001S34D06.wav").as_deref(), Some("audio__0001"));
        assert_eq!(take_key("TASCAM_0030S12D.wav").as_deref(), Some("tascam_0030"));
        assert_eq!(take_key("TASCAM_0030S1.wav").as_deref(), Some("tascam_0030"));
        assert_eq!(take_key("ZOOM0001_Tr1.WAV").as_deref(), Some("zoom0001"));
        assert_eq!(take_key("ZOOM0001_TrLR.WAV").as_deref(), Some("zoom0001"));
        assert_eq!(take_key("Interview_safety.wav").as_deref(), Some("interview"));
        for n in ["TASCAM_0030.wav", "C70_690.mxf", "EOSR_0295.mp4", "DJI_03_20241106_133350.WAV", "IMG_9402.MOV", "GX010045.MP4"] {
            assert_eq!(take_key(n), None, "{n}");
        }
    }


    #[test]
    fn natural() {
        let mut v = vec!["C70_10.mxf", "C70_9.mxf", "C70_100.mxf", "a2", "A1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["A1", "a2", "C70_9.mxf", "C70_10.mxf", "C70_100.mxf"]);
    }
}
