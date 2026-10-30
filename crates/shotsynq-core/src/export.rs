//! Timeline export: FCP7 XML (Resolve / Premiere) and FCPXML. Port of exporter.py.
//! FCP7 output matches Python's ElementTree (indent with two spaces) byte for
//! byte, apart from the random IDs. FCPXML uses asset-clip/media-rep (valid 1.10).

use crate::model::{natural_cmp, Clip, Track};
use std::collections::HashSet;

// ── Minimal XML tree with ElementTree-compatible serialisation ──────────────

pub struct El {
    tag: String,
    attrs: Vec<(String, String)>,
    text: Option<String>,
    children: Vec<El>,
}

impl El {
    pub fn new(tag: &str) -> Self {
        El { tag: tag.into(), attrs: vec![], text: None, children: vec![] }
    }
    fn attr(mut self, k: &str, v: impl Into<String>) -> Self {
        self.attrs.push((k.into(), v.into()));
        self
    }
    fn add(&mut self, child: El) -> &mut El {
        self.children.push(child);
        self.children.last_mut().unwrap()
    }
    fn sub(&mut self, tag: &str) -> &mut El {
        self.add(El::new(tag))
    }
    fn text(&mut self, tag: &str, t: impl Into<String>) {
        let mut e = El::new(tag);
        e.text = Some(t.into());
        self.children.push(e);
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, level: usize) {
        out.push('<');
        out.push_str(&self.tag);
        for (k, v) in &self.attrs {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(&esc_attr(v));
            out.push('"');
        }
        let has_text = self.text.as_deref().is_some_and(|t| !t.is_empty());
        if !has_text && self.children.is_empty() {
            out.push_str(" />");
            return;
        }
        out.push('>');
        if has_text {
            out.push_str(&esc_text(self.text.as_deref().unwrap()));
        }
        if !self.children.is_empty() {
            for c in &self.children {
                out.push('\n');
                out.push_str(&"  ".repeat(level + 1));
                c.write(out, level + 1);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(level));
        }
        out.push_str("</");
        out.push_str(&self.tag);
        out.push('>');
    }
}

fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn esc_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#09;")
}

// ── Helpers ──────────────────────────────────────────────────────────────────

pub type IdGen<'a> = &'a mut dyn FnMut() -> String;

pub fn random_id() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

fn sequence_fps(tracks: &[Track]) -> f64 {
    tracks.iter().flat_map(|t| &t.clips).find_map(|c| c.frame_rate.filter(|&f| f != 0.0)).unwrap_or(25.0)
}

fn timebase_ntsc(fps: f64) -> (i64, bool) {
    let tb = fps.round_ties_even() as i64;
    (tb, (fps - tb as f64).abs() > 0.001)
}

fn fcpxml_fraction(fps: f64) -> (i64, i64) {
    if (fps - 23.976).abs() < 0.01 {
        (24000, 1001)
    } else if (fps - 29.97).abs() < 0.01 {
        (30000, 1001)
    } else if (fps - 59.94).abs() < 0.01 {
        (60000, 1001)
    } else {
        (fps.round_ties_even() as i64, 1)
    }
}

fn frames(seconds: f64, fps: f64) -> i64 {
    (seconds * fps).round_ties_even() as i64
}

fn tf(b: bool) -> &'static str {
    if b { "TRUE" } else { "FALSE" }
}

/// file://localhost/ URL, percent-encoded like Python's quote(safe="/:").
pub fn make_url(path: &str) -> String {
    let abs = std::path::absolute(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path.to_string());
    let forward = abs.replace('\\', "/");
    let mut enc = String::new();
    for b in forward.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "_.-~/:".contains(c) {
            enc.push(c);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    // Python: "file://localhost/" + quote(abs). On Windows abs starts "C:/",
    // on POSIX it starts "/", giving "file://localhost//..." — kept as-is.
    format!("file://localhost/{enc}")
}

struct Ch {
    source: usize,
    depth: u32,
    layout: &'static str,
}

fn channels(c: &Clip) -> Vec<Ch> {
    let n = c.num_audio_channels as usize;
    (0..n)
        .map(|i| Ch { source: i + 1, depth: if n > 2 { 24 } else { 16 }, layout: if n == 2 { "stereo" } else { "mono" } })
        .collect()
}

fn file_element(file_id: &str, c: &Clip, fps: f64) -> El {
    let (tb, ntsc) = timebase_ntsc(fps);
    let mut f = El::new("file").attr("id", file_id);
    f.text("name", &c.name);
    let rate = f.sub("rate");
    rate.text("ntsc", tf(ntsc));
    rate.text("timebase", tb.to_string());
    let dur = frames(c.duration, fps);
    f.text("duration", dur.to_string());
    let media = f.sub("media");
    if c.is_video {
        let v = media.sub("video");
        let sc = v.sub("samplecharacteristics");
        sc.text("width", c.width.to_string());
        sc.text("height", c.height.to_string());
        sc.text("anamorphic", "FALSE");
        sc.text("pixelaspectratio", "square");
        v.text("duration", dur.to_string());
    }
    for ch in channels(c) {
        let a = media.sub("audio");
        a.text("channelcount", "1");
        let sc = a.sub("samplecharacteristics");
        sc.text("depth", ch.depth.to_string());
        sc.text("samplerate", "48000");
        a.text("layout", ch.layout);
        let ac = a.sub("audiochannel");
        ac.text("sourcechannel", ch.source.to_string());
    }
    let tc = f.sub("timecode");
    tc.text("string", &c.timecode);
    tc.text("displayformat", "NDF");
    f.text("pathurl", make_url(&c.path));
    f
}

fn link(parent: &mut El, media: &str, track: impl ToString, clip: impl ToString, group: impl ToString, r: &str) {
    let l = parent.sub("link");
    l.text("mediatype", media);
    l.text("trackindex", track.to_string());
    l.text("clipindex", clip.to_string());
    l.text("groupindex", group.to_string());
    l.text("linkclipref", r);
}

/// Unsynced clips go after the synced region (same rules as exporter.py).
pub fn move_unsynced_to_end(tracks: &mut [Track]) {
    let synced_end = tracks
        .iter()
        .flat_map(|t| &t.clips)
        .filter(|c| c.synced)
        .map(|c| c.offset + c.duration)
        .fold(f64::NEG_INFINITY, f64::max);
    if synced_end == f64::NEG_INFINITY {
        return;
    }
    let mut cursor = synced_end + 2.0;
    for t in tracks.iter_mut() {
        let mut idx: Vec<usize> = (0..t.clips.len()).collect();
        idx.sort_by(|&a, &b| natural_cmp(&t.clips[a].name, &t.clips[b].name));
        if !t.synced {
            let mut running = 0.0;
            for i in idx {
                t.clips[i].offset = cursor + running;
                running += t.clips[i].duration;
            }
            if running > 0.0 {
                cursor += running + 2.0;
            }
        } else {
            let failed: Vec<usize> = idx.into_iter().filter(|&i| !t.clips[i].synced).collect();
            for i in failed {
                t.clips[i].offset = cursor;
                cursor += t.clips[i].duration + 0.5;
            }
        }
    }
}

fn by_offset(t: &Track) -> Vec<&Clip> {
    let mut v: Vec<&Clip> = t.clips.iter().collect();
    v.sort_by(|a, b| a.offset.partial_cmp(&b.offset).unwrap_or(std::cmp::Ordering::Equal)); // stable, like Python
    v
}

/// Camera clips whose scratch audio can go: those where synced recorder
/// (audio-only) clips cover at least half the clip. With no recorder, or
/// where the recorder wasn't running, camera audio is kept.
pub fn scratch_to_strip(tracks: &[Track]) -> HashSet<String> {
    let mut rec: Vec<(f64, f64)> = tracks
        .iter()
        .filter(|t| !t.is_video)
        .flat_map(|t| &t.clips)
        .filter(|c| c.synced)
        .map(|c| (c.offset, c.offset + c.duration))
        .collect();
    rec.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<(f64, f64)> = vec![];
    for (s, e) in rec {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut out = HashSet::new();
    for c in tracks.iter().filter(|t| t.is_video).flat_map(|t| &t.clips).filter(|c| c.synced) {
        let (s, e) = (c.offset, c.offset + c.duration);
        let covered: f64 = merged.iter().map(|&(a, b)| (e.min(b) - s.max(a)).max(0.0)).sum();
        if c.duration > 0.0 && covered >= 0.5 * c.duration {
            out.insert(c.path.clone());
        }
    }
    out
}

// ── FCP7 XML (Resolve / Premiere) ───────────────────────────────────────────

/// `strip`: camera clips (by path) whose scratch audio is left out of the
/// sequence. Bins always keep full media.
pub fn build_fcp7_xml(tracks: &[Track], project_name: &str, id: IdGen, strip: &HashSet<String>) -> Option<String> {
    let all: Vec<&Clip> = tracks.iter().flat_map(|t| &t.clips).collect();
    if all.is_empty() {
        return None;
    }
    let total = all.iter().map(|c| c.offset + c.duration).fold(f64::NEG_INFINITY, f64::max);
    let fps = sequence_fps(tracks);
    let (tb, ntsc) = timebase_ntsc(fps);

    use std::collections::HashMap;
    let mut file_ids: HashMap<&str, String> = HashMap::new();
    let mut master_ids: HashMap<&str, String> = HashMap::new();
    for c in &all {
        if !file_ids.contains_key(c.path.as_str()) {
            file_ids.insert(&c.path, id());
            master_ids.insert(&c.path, id());
        }
    }
    let mut layout: Vec<(usize, usize)> = vec![]; // (track idx, channel idx)
    let keep_audio = |c: &Clip| !(c.is_video && strip.contains(&c.path));
    for (ti, t) in tracks.iter().enumerate() {
        if t.is_video && !t.clips.iter().any(|c| keep_audio(c)) {
            continue;
        }
        let n = t.clips.iter().map(|c| channels(c).len()).max().unwrap_or(1);
        for ch in 0..n {
            layout.push((ti, ch));
        }
    }
    let audio_idx = |ti: usize, ch: usize| -> usize {
        layout.iter().position(|&(t, c)| t == ti && c == ch).map(|p| p + 1).unwrap_or(ch + 1)
    };
    // Clip index of each clip within its sequence track (links refer to these)
    let mut v_ci: HashMap<&str, usize> = HashMap::new();
    for t in tracks.iter().filter(|t| t.is_video) {
        for (k, c) in by_offset(t).into_iter().enumerate() {
            v_ci.insert(&c.path, k + 1);
        }
    }
    let mut a_ci: HashMap<(&str, usize), usize> = HashMap::new();
    for &(ti, ch) in &layout {
        let mut k = 1;
        for c in by_offset(&tracks[ti]) {
            if ch < channels(c).len() && keep_audio(c) {
                a_ci.insert((c.path.as_str(), ch), k);
                k += 1;
            }
        }
    }
    let mut seq_v: HashMap<&str, String> = HashMap::new();
    let mut seq_a: HashMap<&str, Vec<String>> = HashMap::new();
    for c in &all {
        seq_v.insert(&c.path, id());
        seq_a.insert(&c.path, channels(c).iter().map(|_| id()).collect());
    }

    let mut root = El::new("xmeml").attr("version", "4");
    let project = root.sub("project");
    project.text("name", project_name);
    let children = project.sub("children");

    // Bins
    for t in tracks {
        let bin = children.sub("bin");
        bin.text("name", &t.display_name);
        let bc = bin.sub("children");
        let mut clips: Vec<&Clip> = t.clips.iter().collect();
        clips.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        for c in clips {
            let mid = master_ids[c.path.as_str()].clone();
            let fid = file_ids[c.path.as_str()].clone();
            let chs = channels(c);
            let dur = frames(c.duration, fps);
            let master = bc.add(El::new("clip").attr("id", mid.clone()));
            master.text("name", &c.name);
            master.text("duration", dur.to_string());
            let rate = master.sub("rate");
            rate.text("ntsc", tf(ntsc));
            rate.text("timebase", tb.to_string());
            master.text("masterclipid", mid.clone());
            master.text("ismasterclip", "TRUE");
            let media = master.sub("media");
            let v_item = id();
            let a_items: Vec<String> = chs.iter().map(|_| id()).collect();
            if c.is_video {
                let video = media.sub("video");
                let vt = video.sub("track");
                let vi = vt.add(El::new("clipitem").attr("id", v_item.clone()));
                vi.text("masterclipid", mid.clone());
                vi.add(file_element(&fid, c, fps));
                for (j, a) in a_items.iter().enumerate() {
                    link(vi, "audio", j + 1, 1, 1, a);
                }
                vt.text("enabled", "TRUE");
                vt.text("locked", "FALSE");
            }
            let audio = media.sub("audio");
            for a_id in &a_items {
                let at = audio.sub("track");
                let ai = at.add(El::new("clipitem").attr("id", a_id.clone()));
                ai.text("masterclipid", mid.clone());
                ai.add(file_element(&fid, c, fps));
                if c.is_video {
                    link(ai, "video", 1, 1, 1, &v_item);
                }
                for (k, other) in a_items.iter().enumerate() {
                    link(ai, "audio", k + 1, 1, 1, other);
                }
                at.text("enabled", "TRUE");
                at.text("locked", "FALSE");
            }
        }
    }

    // Sequence
    let seq_id = id();
    let seq = children.add(El::new("sequence").attr("id", seq_id.clone()));
    seq.text("name", "Synced Sequence");
    seq.text("duration", frames(total, fps).to_string());
    let r = seq.sub("rate");
    r.text("ntsc", tf(ntsc));
    r.text("timebase", tb.to_string());
    let marker = seq.sub("marker");
    marker.text("name", "shotsynq 000");
    marker.text("in", "0");
    marker.text("out", "-1");
    let smedia = seq.sub("media");
    let mut svideo = El::new("video");
    let mut saudio = El::new("audio");

    let video_tracks: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].is_video).collect();
    for (vti, &ti) in video_tracks.iter().enumerate() {
        let vtn = vti + 1;
        let t = &tracks[ti];
        let vt = svideo.sub("track");
        for (ci, c) in by_offset(t).into_iter().enumerate() {
            let ci = ci + 1;
            let p = c.path.as_str();
            let dur = frames(c.duration, fps);
            let vi = vt.add(El::new("clipitem").attr("id", seq_v[p].clone()));
            vi.text("name", &c.name);
            vi.text("duration", dur.to_string());
            vi.text("in", "0");
            vi.text("out", dur.to_string());
            vi.text("start", frames(c.offset, fps).to_string());
            vi.text("end", frames(c.offset + c.duration, fps).to_string());
            vi.text("masterclipid", master_ids[p].clone());
            vi.add(file_element(&file_ids[p], c, fps));
            let st = vi.sub("sourcetrack");
            st.text("mediatype", "video");
            st.text("trackindex", "1");
            link(vi, "video", vtn, ci, vtn, &seq_v[p]);
            if keep_audio(c) {
                for (ch, a_id) in seq_a[p].iter().enumerate() {
                    link(vi, "audio", audio_idx(ti, ch), a_ci.get(&(p, ch)).copied().unwrap_or(ci), vtn, a_id);
                }
            }
        }
        vt.text("enabled", "TRUE");
        vt.text("locked", "FALSE");
    }
    let first_video = all.iter().find(|c| c.is_video);
    let fmt = svideo.sub("format");
    let sc = fmt.sub("samplecharacteristics");
    sc.text("width", first_video.map(|c| c.width).unwrap_or(1920).to_string());
    sc.text("height", first_video.map(|c| c.height).unwrap_or(1080).to_string());
    sc.text("anamorphic", "FALSE");
    sc.text("pixelaspectratio", "square");
    let fr = sc.sub("rate");
    fr.text("ntsc", tf(ntsc));
    fr.text("timebase", tb.to_string());

    for &(ti, ch) in &layout {
        let t = &tracks[ti];
        let at = saudio.sub("track");
        let vtn = video_tracks.iter().position(|&v| v == ti).map(|p| p + 1).unwrap_or(1);
        for c in by_offset(t) {
            if ch >= channels(c).len() || !keep_audio(c) {
                continue;
            }
            let p = c.path.as_str();
            let ci = a_ci[&(p, ch)];
            let dur = frames(c.duration, fps);
            let ai = at.add(El::new("clipitem").attr("id", seq_a[p][ch].clone()));
            ai.text("name", &c.name);
            ai.text("duration", dur.to_string());
            ai.text("in", "0");
            ai.text("out", dur.to_string());
            ai.text("start", frames(c.offset, fps).to_string());
            ai.text("end", frames(c.offset + c.duration, fps).to_string());
            ai.text("masterclipid", master_ids[p].clone());
            ai.add(file_element(&file_ids[p], c, fps));
            let st = ai.sub("sourcetrack");
            st.text("mediatype", "audio");
            st.text("trackindex", (ch + 1).to_string());
            if c.is_video {
                link(ai, "video", vtn, v_ci.get(p).copied().unwrap_or(ci), vtn, &seq_v[p]);
            }
            for (k, other) in seq_a[p].iter().enumerate() {
                link(ai, "audio", audio_idx(ti, k), a_ci.get(&(p, k)).copied().unwrap_or(ci), vtn, other);
            }
        }
        at.text("enabled", "TRUE");
        at.text("locked", "FALSE");
    }
    smedia.add(svideo);
    smedia.add(saudio);
    seq.text("uuid", seq_id);

    let io = root.sub("importoptions");
    io.text("createnewproject", "FALSE");
    io.text("targetprojectname", project_name);
    io.text("defsequencepresetname", "useFirstClipSettings");
    io.text("displaynonfatalerrors", "TRUE");
    io.text("filterreconnectmediafiles", "TRUE");
    io.text("filterincludemarkers", "TRUE");
    io.text("filterincludeeffects", "TRUE");
    io.text("filterincludesequencesettings", "FALSE");

    Some(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n{}", root.serialize()))
}

// ── FCPXML ───────────────────────────────────────────────────────────────────

pub fn build_fcpxml(tracks: &[Track], project_name: &str, id: IdGen, strip: &HashSet<String>) -> Option<String> {
    let all: Vec<&Clip> = tracks.iter().flat_map(|t| &t.clips).collect();
    if all.is_empty() {
        return None;
    }
    let total = all.iter().map(|c| c.offset + c.duration).fold(f64::NEG_INFINITY, f64::max);
    let fps = sequence_fps(tracks);
    let (num, den) = fcpxml_fraction(fps);
    let secs = |s: f64| -> String {
        let f = (s * (num as f64 / den as f64)).round_ties_even() as i64;
        format!("{}/{}s", f * den, num)
    };
    let mut root = El::new("fcpxml").attr("version", "1.10");
    let res = root.sub("resources");
    let fv = all.iter().find(|c| c.is_video);
    res.add(
        El::new("format")
            .attr("id", "r0")
            .attr("name", format!("FFVideoFormat1080p{}", fps.round_ties_even() as i64))
            .attr("frameDuration", format!("{den}/{num}s"))
            .attr("width", fv.map(|c| c.width).unwrap_or(1920).to_string())
            .attr("height", fv.map(|c| c.height).unwrap_or(1080).to_string()),
    );
    let mut asset_ids: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for c in &all {
        if !asset_ids.contains_key(c.path.as_str()) {
            let aid = id();
            let asset = res.add(
                El::new("asset")
                    .attr("id", aid.clone())
                    .attr("name", c.name.clone())
                    .attr("start", "0s")
                    .attr("duration", secs(c.duration))
                    .attr("hasVideo", if c.is_video { "1" } else { "0" })
                    .attr("hasAudio", "1"),
            );
            asset.add(El::new("media-rep").attr("kind", "original-media").attr("src", make_url(&c.path)));
            asset_ids.insert(&c.path, aid);
        }
    }
    let lib = root.sub("library");
    let ev = lib.add(El::new("event").attr("name", project_name));
    let pr = ev.add(El::new("project").attr("name", project_name));
    let seq = pr.add(
        El::new("sequence")
            .attr("duration", secs(total))
            .attr("format", "r0")
            .attr("tcStart", "0s")
            .attr("tcFormat", "NDF"),
    );
    let spine = seq.sub("spine");
    let gap = spine.add(
        El::new("gap")
            .attr("name", "Synced Timeline")
            .attr("offset", "0s")
            .attr("duration", secs(total))
            .attr("start", "0s"),
    );
    let (mut vl, mut al) = (1i64, -1i64);
    for t in tracks {
        let lane = if t.is_video {
            vl += 1;
            vl - 1
        } else {
            al -= 1;
            al + 1
        };
        for c in by_offset(t) {
            let mut el = El::new("asset-clip")
                .attr("name", c.name.clone())
                .attr("ref", asset_ids[c.path.as_str()].clone())
                .attr("lane", lane.to_string())
                .attr("offset", secs(c.offset))
                .attr("duration", secs(c.duration))
                .attr("start", "0s");
            if c.is_video && strip.contains(&c.path) {
                el = el.attr("srcEnable", "video");
            }
            let ce = gap.add(el);
            ce.text("note", &t.display_name);
        }
    }
    Some(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}", root.serialize()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Resolve,
    Premiere,
    Fcpxml,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExportOptions {
    /// Leave camera scratch audio out wherever a recorder covers the clip.
    pub strip_camera_audio: bool,
    /// Leave unsynced clips out instead of placing them at the end.
    pub drop_unsynced: bool,
}

/// Prepare a copy of the tracks for export according to the options.
pub fn prepare(tracks: &[Track], opts: ExportOptions) -> Vec<Track> {
    let mut t = tracks.to_vec();
    if opts.drop_unsynced {
        for tr in &mut t {
            tr.clips.retain(|c| c.synced);
        }
        t.retain(|tr| !tr.clips.is_empty());
    } else {
        move_unsynced_to_end(&mut t);
    }
    t
}

/// Export the tracks to `path`.
pub fn export(tracks: &[Track], fmt: Format, path: &str, opts: ExportOptions) -> Result<(), String> {
    let t = prepare(tracks, opts);
    let strip = if opts.strip_camera_audio { scratch_to_strip(&t) } else { HashSet::new() };
    let mut ids = random_id;
    let xml = match fmt {
        Format::Resolve | Format::Premiere => build_fcp7_xml(&t, "ShotSynq Project", &mut ids, &strip),
        Format::Fcpxml => build_fcpxml(&t, "ShotSynq Project", &mut ids, &strip),
    }
    .ok_or("Nothing to export")?;
    std::fs::write(path, xml).map_err(|e| format!("Could not write {path}: {e}"))
}
