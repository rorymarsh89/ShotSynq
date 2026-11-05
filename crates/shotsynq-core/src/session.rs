//! App state: tracks, grouping files into tracks, applying sync results and
//! building the JSON the UI renders. Port of api.py (minus the GUI plumbing).

use crate::engine::{Placement, SyncInput};

fn group_key(c: &Clip) -> String {
    c.device.clone().map(|d| format!("device:{d}")).unwrap_or_else(|| c.fingerprint.clone())
}

/// Can `clip` go on lane `t` without clashing?
fn lane_fits(t: &Track, clip: &Clip) -> bool {
    // A recorder's dual recordings are exactly the same length; one
    // device can't record them one after another.
    if !clip.is_video && clip.duration > 10.0 && t.clips.iter().any(|o| (o.duration - clip.duration).abs() < 0.005) {
        return false;
    }
    // Only clips that were really rolling at the same time need separate
    // lanes. Cameras disagree on whether their timestamp marks the start or
    // the end of a clip, so it only counts as a clash if the two overlap
    // under both readings.
    // Timecode, where both clips have it, is the better witness: file dates
    // are often just when the files were copied.
    let da = clip.duration;
    t.clips.iter().all(|o| {
        if let (Some(a), Some(b)) = (tc_seconds(clip), tc_seconds(o)) {
            return (a + da).min(b + o.duration) - a.max(b) <= 1.0;
        }
        match (clip.rec_start, o.rec_start) {
            (Some(a), Some(b)) => {
                let db = o.duration;
                let as_start = (a + da).min(b + db) - a.max(b);
                let as_end = a.min(b) - (a - da).max(b - db);
                as_start.min(as_end) <= 1.0
            }
            _ => true,
        }
    })
}

/// Start timecode in seconds ("HH:MM:SS:FF", ";" for drop frame), if the
/// clip carries one.
fn tc_seconds(c: &Clip) -> Option<f64> {
    if !c.has_timecode {
        return None;
    }
    let parts: Vec<f64> = c.timecode.split([':', ';', '.']).map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let [h, m, s, f] = parts[..] else { return None };
    let fps = c.frame_rate.filter(|&r| r > 1.0).unwrap_or(25.0).round();
    Some(h * 3600.0 + m * 60.0 + s + f / fps)
}

use crate::model::{natural_cmp, Clip, Features, Track, TRACK_COLORS};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CONFIDENCE_OK: f64 = 0.5;

#[derive(Default)]
pub struct Session {
    pub tracks: Vec<Track>,
    camera_counter: usize,
    audio_counter: usize,
    next_track_id: u64,
    /// (batch id, kind, device-or-format) -> candidate track indices
    batch_tracks: HashMap<(u64, bool, String), Vec<usize>>,
    /// Files still being read: path -> batch
    pending: HashMap<String, u64>,
    /// Drops that have been laid out in name order
    finished: HashSet<u64>,
    next_batch: u64,
    pub syncing: bool,
    pub synced: bool,
    /// Wall-clock seconds the last sync took.
    pub last_sync_secs: Option<f64>,
    /// Undo / redo snapshots, newest last
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Last nudge (clip path, when), so a run of nudges is one undo step
    last_nudge: Option<(String, std::time::Instant)>,
}

/// Everything an undo restores: the lanes and whether they're synced.
#[derive(Clone)]
struct Snapshot {
    tracks: Vec<Track>,
    synced: bool,
}

const UNDO_LIMIT: usize = 100;

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a batch of paths; returns (batch id, paths not already added).
    pub fn begin_batch(&mut self, paths: Vec<String>) -> (u64, Vec<String>) {
        let existing: HashSet<&str> = self.tracks.iter().flat_map(|t| &t.clips).map(|c| c.path.as_str()).collect();
        let mut seen = HashSet::new();
        let new: Vec<String> = paths
            .into_iter()
            .filter(|p| !existing.contains(p.as_str()) && !self.pending.contains_key(p) && seen.insert(p.clone()))
            .collect();
        if !new.is_empty() {
            self.checkpoint(); // undo removes the files again
        }
        for p in &new {
            self.pending.insert(p.clone(), self.next_batch + 1);
        }
        self.next_batch += 1;
        (self.next_batch, new)
    }

    pub fn load_failed(&mut self, path: &str) {
        self.pending.remove(path);
        self.finish_batches();
    }

    /// Metadata is ready: put the clip on the right track.
    ///
    /// Files group by device model when known ("Apple iPhone 15 Pro"),
    /// otherwise by format, so one phone shooting different resolutions stays
    /// on one track. Two clips that were recording at the same moment can't
    /// come from one device, so they're split onto separate tracks — this keeps
    /// two identical camera bodies apart.
    pub fn add_clip(&mut self, batch: u64, clip: Clip) {
        self.pending.remove(&clip.path);
        let mut clip = clip;
        clip.batch = batch;
        let group = group_key(&clip);
        let key = (batch, clip.is_video, group);
        let candidates = self.batch_tracks.get(&key).cloned().unwrap_or_default();
        let existing = candidates.iter().copied().find(|&ti| ti < self.tracks.len() && lane_fits(&self.tracks[ti], &clip));
        let ti = match existing {
            Some(ti) => ti,
            None => {
                let name = if clip.is_video {
                    self.camera_counter += 1;
                    format!("Camera {}", self.camera_counter)
                } else {
                    self.audio_counter += 1;
                    format!("Recorder {}", self.audio_counter)
                };
                let color = TRACK_COLORS[self.tracks.len() % TRACK_COLORS.len()].to_string();
                // Cameras first, then recorders, whatever order files finish loading in
                let ti = if clip.is_video {
                    self.tracks.iter().take_while(|t| t.is_video).count()
                } else {
                    self.tracks.len()
                };
                let mut track = Track::new(name, clip.fingerprint.clone(), clip.is_video, color);
                self.next_track_id += 1;
                track.id = self.next_track_id;
                self.tracks.insert(ti, track);
                self.reindex_batches(|i| Some(if i >= ti { i + 1 } else { i }));
                self.batch_tracks.entry(key).or_default().push(ti);
                ti
            }
        };
        let t = &mut self.tracks[ti];
        let mut clip = clip;
        clip.device_lane = t.id;
        t.clips.push(clip);
        t.clips.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    }

    pub fn set_audio(
        &mut self,
        path: &str,
        waveform: Vec<f32>,
        features: Option<Arc<Features>>,
        audio_note: Option<String>,
        error: Option<String>,
    ) {
        for c in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if c.path == path {
                c.waveform = waveform;
                c.features = features;
                c.audio_note = audio_note;
                c.load_error = error;
                c.audio_loaded = true;
                break;
            }
        }
        self.finish_batches();
    }

    /// Run `finish_batch` for every drop that has fully loaded.
    fn finish_batches(&mut self) {
        let mut ids: Vec<u64> = self.tracks.iter().flat_map(|t| &t.clips).map(|c| c.batch).collect();
        ids.sort_unstable();
        ids.dedup();
        for b in ids {
            let loading = self.pending.values().any(|&pb| pb == b)
                || self.tracks.iter().flat_map(|t| &t.clips).any(|c| c.batch == b && !c.audio_loaded);
            if !loading && self.finished.insert(b) {
                self.finish_batch(b);
            }
        }
    }

    /// A drop has fully loaded: lay its audio out again in name order, so
    /// lanes come out the same whatever order the files finished loading in
    /// (a recorder's dual files then line up lane by lane: take 1 and take 2
    /// mains on one lane, the second copies on the next).
    fn finish_batch(&mut self, b: u64) {
        self.batch_tracks.retain(|k, _| k.0 != b);
        let lane_ids: Vec<u64> = self
            .tracks
            .iter()
            .filter(|t| !t.is_video && t.clips.iter().any(|c| c.batch == b))
            .map(|t| t.id)
            .collect();
        if lane_ids.is_empty() {
            return;
        }
        let mut lane_key: HashMap<u64, String> = HashMap::new();
        let mut clips: Vec<Clip> = vec![];
        for t in self.tracks.iter_mut().filter(|t| lane_ids.contains(&t.id)) {
            lane_key.insert(t.id, t.clips.first().map(group_key).unwrap_or_default());
            clips.append(&mut t.clips);
        }
        clips.sort_by(|a, c| natural_cmp(&a.name, &c.name));
        let mut lanes = lane_ids;
        for clip in clips {
            let key = group_key(&clip);
            let id = lanes
                .iter()
                .copied()
                .find(|id| lane_key.get(id) == Some(&key) && self.track_by_id(*id).map_or(false, |t| lane_fits(t, &clip)));
            let id = id.unwrap_or_else(|| {
                let after = lanes.iter().rev().find(|id| lane_key.get(id) == Some(&key)).or(lanes.last()).copied();
                let at = after.and_then(|a| self.index_of(a)).map_or(self.tracks.len(), |i| i + 1);
                let id = self.insert_lane(at, &clip);
                lane_key.insert(id, key.clone());
                lanes.push(id);
                id
            });
            let ti = self.index_of(id).unwrap();
            let mut clip = clip;
            clip.device_lane = id; // the final split decides the device lane
            self.tracks[ti].clips.push(clip);
        }
        // Drop this drop's lanes left empty by the relayout (not tracks the
        // user emptied), then renumber generated names
        let mut i = self.tracks.len();
        while i > 0 {
            i -= 1;
            if !self.tracks[i].is_video && !self.tracks[i].user_lane && self.tracks[i].clips.is_empty() && lanes.contains(&self.tracks[i].id) {
                self.tracks.remove(i);
                self.reindex_batches(|ti| if ti == i { None } else if ti > i { Some(ti - 1) } else { Some(ti) });
            }
        }
        self.renumber_lanes();
        self.recolour();
    }

    /// Generated names follow lane order: Camera 1, Camera 2… Recorder 1…
    fn renumber_lanes(&mut self) {
        for video in [true, false] {
            let mut k = 0;
            for t in self.tracks.iter_mut().filter(|t| t.is_video == video && !t.user_lane) {
                k += 1;
                if t.auto_name {
                    t.display_name = format!("{} {k}", if video { "Camera" } else { "Recorder" });
                }
            }
            if video { self.camera_counter = k } else { self.audio_counter = k }
        }
    }

    /// Colours follow lane position after loading, not load order.
    fn recolour(&mut self) {
        for (i, t) in self.tracks.iter_mut().enumerate() {
            t.color = TRACK_COLORS[i % TRACK_COLORS.len()].to_string();
        }
    }

    fn track_by_id(&self, id: u64) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    fn insert_lane(&mut self, at: usize, clip: &Clip) -> u64 {
        let color = TRACK_COLORS[self.tracks.len() % TRACK_COLORS.len()].to_string();
        let mut t = Track::new(String::new(), clip.fingerprint.clone(), false, color);
        self.next_track_id += 1;
        t.id = self.next_track_id;
        self.tracks.insert(at, t);
        self.reindex_batches(|i| Some(if i >= at { i + 1 } else { i }));
        self.next_track_id
    }

    pub fn remove_track(&mut self, i: usize) {
        if i < self.tracks.len() {
            self.checkpoint();
            self.tracks.remove(i);
            self.reindex_batches(|ti| if ti == i { None } else if ti > i { Some(ti - 1) } else { Some(ti) });
        }
    }

    // ── Lanes ──────────────────────────────────────────────────────────────

    /// Add an empty lane at the bottom. It takes the kind (camera or audio) of
    /// the first clips moved onto it.
    pub fn add_lane(&mut self) {
        self.checkpoint();
        let n = self.tracks.iter().filter(|t| t.user_lane).count() + 1;
        let color = TRACK_COLORS[self.tracks.len() % TRACK_COLORS.len()].to_string();
        let mut t = Track::new(format!("Track {n}"), String::new(), false, color);
        t.auto_name = false;
        t.user_lane = true;
        self.next_track_id += 1;
        t.id = self.next_track_id;
        self.tracks.push(t);
    }

    /// Move clips to another lane, keeping their time position (so the sync
    /// can't be disturbed). Refused if the kinds don't match or the moved
    /// clips would overlap clips already on that lane. `target` one past the
    /// last track makes a new track for them (as dragging below the last
    /// track does in an editor).
    pub fn move_to_lane(&mut self, clips: &[(usize, usize)], target: usize) -> Result<usize, String> {
        if self.syncing || target > self.tracks.len() {
            return Err("Can't move clips right now".into());
        }
        if target == self.tracks.len() {
            return self.move_to_new_lane(clips);
        }
        self.move_to_lane_inner(clips, target, true)
    }

    fn move_to_lane_inner(&mut self, clips: &[(usize, usize)], target: usize, checkpoint: bool) -> Result<usize, String> {
        let paths: Vec<String> = clips
            .iter()
            .filter_map(|&(ti, ci)| self.tracks.get(ti)?.clips.get(ci).map(|c| c.path.clone()))
            .filter(|p| !self.tracks[target].clips.iter().any(|c| &c.path == p))
            .collect();
        if paths.is_empty() {
            return Ok(0);
        }
        let moving: Vec<&Clip> = self.tracks.iter().flat_map(|t| &t.clips).filter(|c| paths.contains(&c.path)).collect();
        let video = moving[0].is_video;
        if moving.iter().any(|c| c.is_video != video) {
            return Err("Camera clips and audio clips can't share a track".into());
        }
        let t = &self.tracks[target];
        let empty = t.clips.is_empty();
        if !empty && t.is_video != video {
            return Err(format!(
                "{} is {} track",
                t.display_name,
                if t.is_video { "a camera" } else { "an audio" }
            ));
        }
        // Synced clips keep their time, so they must fit around what's there
        let placed = |c: &Clip| c.synced;
        for m in moving.iter().filter(|c| placed(c)) {
            let others = t.clips.iter().chain(moving.iter().copied().filter(|o| o.path != m.path));
            for o in others.filter(|o| placed(o)) {
                let overlap = (m.offset + m.duration).min(o.offset + o.duration) - m.offset.max(o.offset);
                if overlap > 0.04 {
                    return Err(format!("{} would overlap {} on {}", m.name, o.name, t.display_name));
                }
            }
        }
        if checkpoint {
            self.checkpoint();
        }
        let mut taken = vec![];
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if i == target {
                continue;
            }
            let (go, stay): (Vec<Clip>, Vec<Clip>) = std::mem::take(&mut tr.clips).into_iter().partition(|c| paths.contains(&c.path));
            tr.clips = stay;
            taken.extend(go);
        }
        let n = taken.len();
        let t = &mut self.tracks[target];
        if empty {
            t.is_video = video;
            t.fingerprint = taken[0].fingerprint.clone();
        }
        t.clips.extend(taken);
        t.clips.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        for t in &mut self.tracks {
            t.synced = t.clips.iter().any(|c| c.synced);
        }
        // A track emptied by the move stays (tracks vanishing under the user
        // feels wrong); they can remove it themselves
        Ok(n)
    }

    fn move_to_new_lane(&mut self, clips: &[(usize, usize)]) -> Result<usize, String> {
        let first = clips.iter().find_map(|&(ti, ci)| self.tracks.get(ti)?.clips.get(ci)).cloned();
        let Some(first) = first else { return Ok(0) };
        let video = first.is_video;
        // Moving clips must agree on kind and not overlap each other
        let moving: Vec<&Clip> = clips.iter().filter_map(|&(ti, ci)| self.tracks.get(ti)?.clips.get(ci)).collect();
        if moving.iter().any(|c| c.is_video != video) {
            return Err("Camera clips and audio clips can't share a track".into());
        }
        for (i, m) in moving.iter().enumerate().filter(|(_, c)| c.synced) {
            for o in moving.iter().skip(i + 1).filter(|c| c.synced) {
                let overlap = (m.offset + m.duration).min(o.offset + o.duration) - m.offset.max(o.offset);
                if overlap > 0.04 {
                    return Err(format!("{} and {} overlap, so they can't share a track", m.name, o.name));
                }
            }
        }
        let color = TRACK_COLORS[self.tracks.len() % TRACK_COLORS.len()].to_string();
        let mut t = Track::new(String::new(), first.fingerprint.clone(), video, color);
        self.next_track_id += 1;
        t.id = self.next_track_id;
        // Undo should take away the new track too: checkpoint before adding it
        self.checkpoint();
        self.tracks.push(t);
        let target = self.tracks.len() - 1;
        let r = self.move_to_lane_inner(clips, target, false);
        self.renumber_lanes();
        r
    }

    /// Reorder lanes: move lane `from` to position `to`.
    pub fn move_track(&mut self, from: usize, to: usize) {
        let n = self.tracks.len();
        if from >= n || to >= n || from == to || self.syncing {
            return;
        }
        self.checkpoint();
        let t = self.tracks.remove(from);
        self.tracks.insert(to, t);
        self.reindex_batches(|i| {
            Some(if i == from {
                to
            } else if from < to && i > from && i <= to {
                i - 1
            } else if to < from && i >= to && i < from {
                i + 1
            } else {
                i
            })
        });
    }

    // ── Moving clips by hand ───────────────────────────────────────────────

    /// Place a clip at `offset` (seconds) by hand. It counts as synced and is
    /// pinned to the synced clip on another lane it overlaps most (or the
    /// nearest), so a re-sync keeps it there. `nudge`: part of a run of
    /// keyboard nudges, which undo as one step.
    pub fn move_clip(&mut self, ti: usize, ci: usize, offset: f64, nudge: bool) -> bool {
        let Some(path) = self.tracks.get(ti).and_then(|t| t.clips.get(ci)).map(|c| c.path.clone()) else { return false };
        if self.syncing {
            return false;
        }
        let run = nudge
            && self.last_nudge.as_ref().is_some_and(|(p, t)| *p == path && t.elapsed().as_secs_f64() < 1.5);
        if !run {
            self.checkpoint();
        }
        self.last_nudge = nudge.then(|| (path.clone(), std::time::Instant::now()));
        self.place(ti, ci, offset);
        true
    }

    /// Move several clips at once (a box selection dragged sideways): one
    /// undo step. `moves`: (track, clip, new offset).
    pub fn move_clips(&mut self, moves: &[(usize, usize, f64)]) -> usize {
        let ok: Vec<_> = moves
            .iter()
            .copied()
            .filter(|&(ti, ci, _)| self.tracks.get(ti).is_some_and(|t| ci < t.clips.len()))
            .collect();
        if self.syncing || ok.is_empty() {
            return 0;
        }
        self.checkpoint();
        self.last_nudge = None;
        for &(ti, ci, offset) in &ok {
            self.place(ti, ci, offset);
        }
        ok.len()
    }

    fn place(&mut self, ti: usize, ci: usize, offset: f64) {
        let dur = self.tracks[ti].clips[ci].duration;
        let device = self.tracks[ti].clips[ci].device_lane;
        let offset = offset.max(0.0);
        // Anchor: another device's synced clip, most overlap first, then nearest
        let mut best: Option<(f64, f64, String, f64)> = None; // (overlap, -distance, path, start)
        for (tj, t) in self.tracks.iter().enumerate() {
            for c in t.clips.iter().filter(|c| c.synced && !c.manual) {
                if (device != 0 && c.device_lane == device) || (device == 0 && tj == ti) {
                    continue;
                }
                let overlap = (offset + dur).min(c.offset + c.duration) - offset.max(c.offset);
                let dist = if overlap > 0.0 { 0.0 } else { -overlap };
                let key = (overlap.max(0.0), -dist);
                if best.as_ref().map_or(true, |b| key > (b.0, b.1)) {
                    best = Some((key.0, key.1, c.path.clone(), c.offset));
                }
            }
        }
        let c = &mut self.tracks[ti].clips[ci];
        // A clip that was already synced keeps its match strength, so a weak
        // match being nudged stays "to check" until the user accepts it
        if !c.synced {
            c.confidence = 1.0;
        }
        c.offset = offset;
        c.synced = true;
        c.manual = true;
        c.excluded = false;
        c.sync_tried = true;
        c.sync_note = None;
        c.pin = best.map(|(_, _, p, start)| (p, offset - start));
        self.tracks[ti].synced = true;
        self.synced = true;
    }

    /// Accept a weak match as right.
    pub fn confirm_clip(&mut self, ti: usize, ci: usize) {
        if self.tracks.get(ti).is_some_and(|t| ci < t.clips.len()) {
            self.checkpoint();
            self.tracks[ti].clips[ci].confirmed = true;
        }
    }

    /// Accept every weak match at once.
    pub fn confirm_all(&mut self) -> usize {
        let n = self.tracks.iter().flat_map(|t| &t.clips).filter(|c| self.clip_status(c) == "synced_low").count();
        if n == 0 {
            return 0;
        }
        self.checkpoint();
        for c in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if c.synced && c.confidence < CONFIDENCE_OK {
                c.confirmed = true;
            }
        }
        n
    }

    /// The match is wrong: take the clip off the timeline and keep it out of
    /// syncing until it's placed by hand.
    pub fn unsync_clip(&mut self, ti: usize, ci: usize) {
        if self.syncing || !self.tracks.get(ti).is_some_and(|t| ci < t.clips.len()) {
            return;
        }
        self.checkpoint();
        let c = &mut self.tracks[ti].clips[ci];
        c.synced = false;
        c.manual = false;
        c.pin = None;
        c.confirmed = false;
        c.excluded = true;
        c.confidence = 0.0;
        c.sync_note = Some("removed by you".into());
        let t = &mut self.tracks[ti];
        t.synced = t.clips.iter().any(|c| c.synced);
    }

    // ── Undo / redo ────────────────────────────────────────────────────────

    fn snapshot(&self) -> Snapshot {
        Snapshot { tracks: self.tracks.clone(), synced: self.synced }
    }

    /// Record the current state so the next change can be undone.
    pub fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last_nudge = None;
    }

    /// Forget the last checkpoint (the change it was for didn't happen).
    pub fn drop_checkpoint(&mut self) {
        self.undo.pop();
    }

    fn restore(&mut self, s: Snapshot) {
        self.tracks = s.tracks;
        self.synced = s.synced;
        self.batch_tracks.clear();
        self.camera_counter = self.tracks.iter().filter(|t| t.is_video).count();
        self.audio_counter = self.tracks.iter().filter(|t| !t.is_video).count();
        self.last_nudge = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() && !self.syncing && self.pending.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty() && !self.syncing && self.pending.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        if !self.can_undo() {
            return false;
        }
        let s = self.undo.pop().unwrap();
        self.redo.push(self.snapshot());
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.can_redo() {
            return false;
        }
        let s = self.redo.pop().unwrap();
        self.undo.push(self.snapshot());
        self.restore(s);
        true
    }

    /// Frame rate for nudging: the first camera's, else 25.
    pub fn timeline_fps(&self) -> f64 {
        self.tracks
            .iter()
            .filter(|t| t.is_video)
            .flat_map(|t| &t.clips)
            .find_map(|c| c.frame_rate)
            .filter(|&f| f > 1.0)
            .unwrap_or(25.0)
    }

    fn reindex_batches(&mut self, f: impl Fn(usize) -> Option<usize>) {
        self.batch_tracks = self
            .batch_tracks
            .drain()
            .map(|(k, v)| (k, v.into_iter().filter_map(&f).collect::<Vec<_>>()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
    }

    pub fn rename_track(&mut self, i: usize, name: String) {
        if self.tracks.get(i).is_some_and(|t| t.display_name != name) {
            self.checkpoint();
            let t = &mut self.tracks[i];
            t.display_name = name;
            t.auto_name = false;
        }

    }

    pub fn delete_clip(&mut self, ti: usize, ci: usize) {
        if self.tracks.get(ti).is_some_and(|t| ci < t.clips.len()) {
            self.checkpoint();
            self.tracks[ti].clips.remove(ci);
        }
        self.finish_batches();
    }

    /// Remove several clips as one undo step.
    pub fn delete_clips(&mut self, clips: &[(usize, usize)]) {
        let paths: HashSet<String> = clips
            .iter()
            .filter_map(|&(ti, ci)| self.tracks.get(ti)?.clips.get(ci).map(|c| c.path.clone()))
            .collect();
        if paths.is_empty() {
            return;
        }
        self.checkpoint();
        for t in &mut self.tracks {
            t.clips.retain(|c| !paths.contains(&c.path));
        }
        self.finish_batches();
    }

    /// Remove every clip that failed to sync (after a sync).
    pub fn remove_unsynced(&mut self) -> usize {
        if !self.synced {
            return 0;
        }
        self.checkpoint();
        let before: usize = self.tracks.iter().map(|t| t.clips.len()).sum();
        for t in &mut self.tracks {
            t.clips.retain(|c| c.synced || !c.sync_tried); // keep files added since
        }
        let removed = before - self.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
        if removed == 0 {
            self.drop_checkpoint();
        }
        removed
    }

    /// Clear everything. Undoable (the files are still on disk).
    pub fn clear(&mut self) {
        self.checkpoint();
        let undo = std::mem::take(&mut self.undo);
        *self = Session { next_batch: self.next_batch, undo, ..Default::default() };
    }

    pub fn can_sync(&self) -> bool {
        let total = self.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
        let loaded = self.tracks.iter().flat_map(|t| &t.clips).filter(|c| c.audio_loaded).count();
        total > 0 && loaded == total && self.tracks.len() >= 2 && !self.syncing && self.pending.is_empty()
    }

    /// Inputs for the engine plus (track, clip) index for each.
    pub fn sync_inputs(&self) -> (Vec<SyncInput>, Vec<(usize, usize)>) {
        let mut inputs = vec![];
        let mut map = vec![];
        // The engine's "track" is the device, not the lane the clip sits on now
        let mut devices: HashMap<u64, usize> = HashMap::new();
        for (ti, t) in self.tracks.iter().enumerate() {
            for (ci, c) in t.clips.iter().enumerate() {
                let key = if c.device_lane != 0 { c.device_lane } else { u64::MAX - ti as u64 };
                let n = devices.len();
                let device = *devices.entry(key).or_insert(n);
                inputs.push(SyncInput {
                    name: c.name.clone(),
                    track: device,
                    duration: c.duration,
                    creation_time: c.creation_time.clone(),
                    features: if c.excluded { None } else { c.features.clone() },
                    take: c.take_key.clone(),
                    pin: None,
                });
                map.push((ti, ci));
            }
        }
        // Hand placements: pin each to its anchor clip
        let index: HashMap<&str, usize> =
            map.iter().enumerate().map(|(k, &(ti, ci))| (self.tracks[ti].clips[ci].path.as_str(), k)).collect();
        for (k, &(ti, ci)) in map.iter().enumerate() {
            if let Some((anchor, d)) = &self.tracks[ti].clips[ci].pin {
                inputs[k].pin = index.get(anchor.as_str()).map(|&a| (a, *d));
            }
        }
        (inputs, map)
    }

    /// Apply results; `paths` guards against the user editing tracks mid-sync.
    pub fn apply_placements(&mut self, map: &[(usize, usize)], paths: &[String], placements: &[Placement]) -> bool {
        for ((ti, ci), p) in map.iter().zip(paths) {
            if self.tracks.get(*ti).and_then(|t| t.clips.get(*ci)).map(|c| &c.path) != Some(p) {
                return false;
            }
        }
        let mut moved = vec![];
        for (&(ti, ci), p) in map.iter().zip(placements) {
            let c = &mut self.tracks[ti].clips[ci];
            if c.manual && c.pin.is_none() {
                moved.push((ti, ci));
                continue; // placed by hand with nothing to pin to: leave it be
            }
            // A weak match the user accepted stays accepted if it lands in
            // the same place again
            c.confirmed = c.confirmed && c.synced && p.synced && (p.offset - c.offset).abs() < 0.04;
            c.offset = p.offset;
            c.synced = p.synced;
            c.confidence = p.confidence;
            c.sync_tried = true;
            c.sync_note = p.edited.then(|| "edited export, not raw footage".to_string());
        }
        let min = self.tracks.iter().flat_map(|t| &t.clips).map(|c| c.offset).fold(f64::INFINITY, f64::min);
        if min < 0.0 {
            for c in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
                c.offset -= min;
            }
        }
        for t in &mut self.tracks {
            t.synced = t.clips.iter().any(|c| c.synced);
            t.confidence = t.clips.iter().map(|c| c.confidence).sum::<f64>() / t.clips.len().max(1) as f64;
        }
        true
    }

    fn clip_status(&self, c: &Clip) -> &'static str {
        if self.syncing {
            return "loading";
        }
        if self.synced && !c.sync_tried {
            return "new";
        }
        if !c.audio_loaded {
            return if self.synced { "failed" } else { "loading" };
        }
        if self.synced {
            if !c.synced {
                return "failed";
            }
            return if c.confidence >= CONFIDENCE_OK || c.confirmed { "synced" } else { "synced_low" };
        }
        "ready"
    }

    pub fn state(&self) -> Value {
        let all: Vec<&Clip> = self.tracks.iter().flat_map(|t| &t.clips).collect();
        let total = all.len();
        let loaded = all.iter().filter(|c| c.audio_loaded).count();
        let n_tracks = self.tracks.len();
        let any_synced = all.iter().any(|c| c.synced);
        // After a sync, unsynced clips sit on their own lane after the synced
        // section, laid out exactly as the export will place them.
        let unsynced: HashMap<(usize, usize), f64> = if any_synced || self.synced {
            crate::export::unsynced_layout(&self.tracks).into_iter().map(|(t, c, o)| ((t, c), o)).collect()
        } else {
            HashMap::new()
        };
        let max_end = if total == 0 {
            0.0
        } else if any_synced || self.synced {
            // Unsynced clips sit after the synced section, as in the export
            let synced_end = all.iter().filter(|c| c.synced).map(|c| c.offset + c.duration).fold(0.0, f64::max);
            unsynced
                .iter()
                .map(|(&(t, c), &o)| o + self.tracks[t].clips[c].duration)
                .fold(synced_end, f64::max)
        } else {
            self.tracks.iter().map(|t| t.clips.iter().map(|c| c.duration).sum::<f64>()).fold(0.0, f64::max)
        };
        let pending = self.pending.len();
        let new = if self.synced { all.iter().filter(|c| !c.sync_tried).count() } else { 0 };
        let (text, kind) = if total == 0 && pending == 0 {
            ("Add media files to get started".to_string(), "default")
        } else if loaded < total || pending > 0 {
            (format!("Loading files… ({loaded}/{})", total + pending), "loading")
        } else if n_tracks < 2 {
            (
                format!(
                    "{total} file{} loaded  —  add files from a second camera or audio recorder",
                    if total != 1 { "s" } else { "" }
                ),
                "default",
            )
        } else if new > 0 && !self.syncing {
            (format!("{new} new file{} · click Re-sync to place {}", if new == 1 { "" } else { "s" }, if new == 1 { "it" } else { "them" }), "default")
        } else if self.synced {
            // After a sync the footer counters say it all
            (String::new(), "default")
        } else {
            (format!("{n_tracks} tracks  —  {total} clips  —  click Synchronise to begin"), "default")
        };

        let tracks: Vec<Value> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let clips: Vec<Value> = t
                    .clips
                    .iter()
                    .enumerate()
                    .map(|(j, c)| {
                        json!({
                            "track_index": i,
                            "clip_index": j,
                            "name": c.name,
                            "duration": c.duration,
                            "offset": c.offset,
                            "unsynced_offset": unsynced.get(&(i, j)),
                            "status": self.clip_status(c),
                            "confidence": c.confidence,
                            "waveform": c.waveform,
                            "frame_rate": c.frame_rate,
                            "width": c.width,
                            "height": c.height,
                            "creation_time": c.creation_time.clone().unwrap_or_default(),
                            "timecode": c.timecode,
                            "has_timecode": c.has_timecode,
                            "manual": c.manual,
                            "confirmed": c.confirmed,
                            "match": if !c.synced { None } else if c.confidence >= 0.8 { Some("strong") } else if c.confidence >= CONFIDENCE_OK { Some("good") } else { Some("check") },
                            "error": c.load_error,
                            "note": if self.synced && !c.sync_tried && !self.syncing {
                                Some(if c.audio_loaded { "new · not synced yet".to_string() } else { "new · loading…".to_string() })
                            } else {
                                c.sync_note.clone().or_else(|| c.audio_note.clone())
                            },
                        })
                    })
                    .collect();
                json!({
                    "index": i,
                    "display_name": t.display_name,
                    "color": t.color,
                    "is_video": t.is_video,
                    "user_lane": t.user_lane,
                    "meta": t.meta_lines(),
                    "clips": clips,
                })
            })
            .collect();

        json!({
            "tracks": tracks,
            "status": {"text": text, "type": kind},
            "buttons": {
                "add": !self.syncing,
                "sync": self.can_sync(),
                "cancel": self.syncing,
                "export": any_synced && !self.syncing,
                "clear": n_tracks > 0 && !self.syncing,
                "undo": self.can_undo(),
                "redo": self.can_redo(),
            },
            "timeline": {
                "total_duration": max_end,
                // A sync has run (even if nothing matched): lay out the result
                "synced": any_synced || self.synced,
                "new": new,
                "fps": self.timeline_fps(),
                "footage": all.iter().map(|c| c.duration).sum::<f64>(),
                "sync_seconds": self.last_sync_secs,
            },
        })
    }

    /// Status line after a finished sync.
    pub fn sync_summary(&self) -> (String, &'static str) {
        let all: Vec<&Clip> = self.tracks.iter().flat_map(|t| &t.clips).collect();
        let check = all.iter().filter(|c| self.clip_status(c) == "synced_low").count();
        let failed = all.iter().filter(|c| !c.synced).count();
        if failed == all.len() {
            return ("No matching sound found between these files · check they recorded the same sound".into(), "warning");
        }
        if check + failed == 0 {
            return (String::new(), "success");
        }
        let mut parts = vec![];
        if check > 0 {
            parts.push(format!("{check} match{} to check", if check == 1 { "" } else { "es" }));
        }
        if failed > 0 {
            parts.push(format!("{failed} unsynced, in red at the end"));
        }
        (parts.join(" · "), "warning")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(start: f64, dur: f64) -> Clip {
        Clip { is_video: true, rec_start: Some(start), duration: dur, ..Default::default() }
    }

    #[test]
    fn back_to_back_clips_share_a_lane() {
        // Canon C70 day: short gaps between takes, timestamps = clip start
        let mut t = Track::new("Camera 1".into(), String::new(), true, String::new());
        let mut at = 0.0;
        for d in [23.5, 93.8, 100.2, 225.0, 85.6, 135.5, 50.3] {
            let c = clip(at, d);
            assert!(lane_fits(&t, &c), "clip at {at}");
            t.clips.push(c);
            at += d + 6.0;
        }
    }

    fn two_lanes() -> Session {
        let mut s = Session::new();
        let mut cam = Track::new("Camera 1".into(), String::new(), true, String::new());
        cam.clips.push(Clip { path: "cam".into(), duration: 60.0, synced: true, is_video: true, ..Default::default() });
        let mut rec = Track::new("Recorder 1".into(), String::new(), false, String::new());
        rec.clips.push(Clip { path: "rec".into(), duration: 30.0, ..Default::default() });
        s.tracks = vec![cam, rec];
        s.synced = true;
        s
    }

    #[test]
    fn move_pins_to_overlapping_clip_and_undoes() {
        let mut s = two_lanes();
        assert!(s.move_clip(1, 0, 12.0, false));
        let c = &s.tracks[1].clips[0];
        assert!(c.synced && c.manual);
        assert_eq!(c.pin, Some(("cam".to_string(), 12.0)));
        assert!(s.undo());
        assert!(!s.tracks[1].clips[0].manual);
        assert!(s.redo());
        assert_eq!(s.tracks[1].clips[0].offset, 12.0);
    }

    #[test]
    fn nudges_undo_as_one_step() {
        let mut s = two_lanes();
        s.move_clip(1, 0, 10.0, false);
        for k in 1..=5 {
            s.move_clip(1, 0, 10.0 + k as f64 * 0.04, true);
        }
        assert!((s.tracks[1].clips[0].offset - 10.2).abs() < 1e-9);
        s.undo(); // all five nudges
        assert_eq!(s.tracks[1].clips[0].offset, 10.0);
        s.undo(); // the drag
        assert!(!s.tracks[1].clips[0].manual);
        assert!(!s.undo());
    }

    fn cams() -> Session {
        // Camera 1 with two takes, Recorder 1 with one
        let mut s = Session::new();
        let mut cam = Track::new("Camera 1".into(), String::new(), true, String::new());
        cam.id = 1;
        for (p, o) in [("c1", 0.0), ("c2", 100.0)] {
            cam.clips.push(Clip { path: p.into(), name: p.into(), offset: o, duration: 50.0, synced: true, is_video: true, device_lane: 1, ..Default::default() });
        }
        let mut rec = Track::new("Recorder 1".into(), String::new(), false, String::new());
        rec.id = 2;
        rec.clips.push(Clip { path: "r1".into(), name: "r1".into(), duration: 60.0, synced: true, device_lane: 2, ..Default::default() });
        s.tracks = vec![cam, rec];
        s.next_track_id = 2;
        s.synced = true;
        s
    }

    #[test]
    fn clips_move_to_a_new_lane_keeping_time_and_device() {
        let mut s = cams();
        s.add_lane();
        assert_eq!(s.move_to_lane(&[(0, 1)], 2), Ok(1));
        let lane = &s.tracks[2];
        assert!(lane.is_video && lane.user_lane);
        assert_eq!(lane.clips[0].offset, 100.0);
        // The engine still sees both takes as one device
        let (inputs, map) = s.sync_inputs();
        let dev = |p: &str| inputs[map.iter().position(|&(t, c)| s.tracks[t].clips[c].path == p).unwrap()].track;
        assert_eq!(dev("c1"), dev("c2"));
        assert_ne!(dev("c1"), dev("r1"));
        assert!(s.undo());
        assert_eq!(s.tracks[0].clips.len(), 2);
    }

    #[test]
    fn lane_moves_refuse_wrong_kind_and_overlaps() {
        let mut s = cams();
        assert!(s.move_to_lane(&[(1, 0)], 0).is_err()); // audio onto camera lane
        s.add_lane();
        s.move_to_lane(&[(0, 0)], 2).unwrap(); // c1 (0-50) to Lane 1
        // r1 is audio: refused on a camera lane even though Lane 1 was new
        assert!(s.move_to_lane(&[(1, 0)], 2).is_err());
    }

    #[test]
    fn overlapping_drop_is_refused() {
        let mut s = cams();
        s.tracks[0].clips[1].offset = 30.0; // c2 30-80 overlaps c1 0-50
        s.add_lane();
        s.move_to_lane(&[(0, 0)], 2).unwrap();
        // c2 (30-80) onto Lane 1, which now holds c1 (0-50)
        let err = s.move_to_lane(&[(0, 0)], 2).unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn lanes_reorder_and_undo() {
        let mut s = cams();
        s.move_track(1, 0);
        assert_eq!(s.tracks[0].display_name, "Recorder 1");
        s.undo();
        assert_eq!(s.tracks[0].display_name, "Camera 1");
    }

    #[test]
    fn timecode_beats_copied_file_dates() {
        // Same "recorded" time on every file (they were copied), different timecode
        let mut t = Track::new("Camera 1".into(), String::new(), true, String::new());
        let mk = |tc: &str| Clip { is_video: true, rec_start: Some(1000.0), duration: 2.0, has_timecode: true,
            timecode: tc.into(), frame_rate: Some(23.976), ..Default::default() };
        t.clips.push(mk("08:08:51:20"));
        assert!(lane_fits(&t, &mk("08:28:00:06")));
        assert!(!lane_fits(&t, &mk("08:08:52:00"))); // really did overlap
    }

    #[test]
    fn simultaneous_clips_get_separate_lanes() {
        let mut t = Track::new("Camera 1".into(), String::new(), true, String::new());
        t.clips.push(clip(1000.0, 120.0));
        assert!(!lane_fits(&t, &clip(1010.0, 115.0)));
    }
}
