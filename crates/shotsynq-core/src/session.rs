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
    let Some(a) = clip.rec_start else { return true };
    let da = clip.duration;
    t.clips.iter().all(|o| match o.rec_start {
        Some(b) => {
            let db = o.duration;
            let as_start = (a + da).min(b + db) - a.max(b);
            let as_end = a.min(b) - (a - da).max(b - db);
            as_start.min(as_end) <= 1.0
        }
        None => true,
    })
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
}

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
            self.tracks[ti].clips.push(clip);
        }
        // Drop lanes left empty, then renumber generated names
        let mut i = self.tracks.len();
        while i > 0 {
            i -= 1;
            if !self.tracks[i].is_video && self.tracks[i].clips.is_empty() {
                self.tracks.remove(i);
                self.reindex_batches(|ti| if ti == i { None } else if ti > i { Some(ti - 1) } else { Some(ti) });
            }
        }
        self.renumber_lanes();
    }

    /// Generated names follow lane order: Camera 1, Camera 2… Recorder 1…
    fn renumber_lanes(&mut self) {
        for video in [true, false] {
            let mut k = 0;
            for t in self.tracks.iter_mut().filter(|t| t.is_video == video) {
                k += 1;
                if t.auto_name {
                    t.display_name = format!("{} {k}", if video { "Camera" } else { "Recorder" });
                }
            }
            if video { self.camera_counter = k } else { self.audio_counter = k }
        }
        // Colours follow lane position, not load order
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
            self.tracks.remove(i);
            self.reindex_batches(|ti| if ti == i { None } else if ti > i { Some(ti - 1) } else { Some(ti) });
        }
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
        if let Some(t) = self.tracks.get_mut(i) {
            t.display_name = name;
            t.auto_name = false;
        }

    }

    pub fn delete_clip(&mut self, ti: usize, ci: usize) {
        if let Some(t) = self.tracks.get_mut(ti) {
            if ci < t.clips.len() {
                t.clips.remove(ci);
            }
        }
        self.finish_batches();
    }

    /// Remove every clip that failed to sync (after a sync).
    pub fn remove_unsynced(&mut self) -> usize {
        if !self.synced {
            return 0;
        }
        let before: usize = self.tracks.iter().map(|t| t.clips.len()).sum();
        for t in &mut self.tracks {
            t.clips.retain(|c| c.synced || !c.sync_tried); // keep files added since
        }
        before - self.tracks.iter().map(|t| t.clips.len()).sum::<usize>()
    }

    pub fn clear(&mut self) {
        *self = Session { next_batch: self.next_batch, ..Default::default() };
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
        for (ti, t) in self.tracks.iter().enumerate() {
            for (ci, c) in t.clips.iter().enumerate() {
                inputs.push(SyncInput {
                    name: c.name.clone(),
                    track: ti,
                    duration: c.duration,
                    creation_time: c.creation_time.clone(),
                    features: c.features.clone(),
                    take: c.take_key.clone(),
                });
                map.push((ti, ci));
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
        for (&(ti, ci), p) in map.iter().zip(placements) {
            let c = &mut self.tracks[ti].clips[ci];
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
            return if c.confidence >= CONFIDENCE_OK { "synced" } else { "synced_low" };
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
            },
            "timeline": {
                "total_duration": max_end,
                "synced": any_synced,
                "new": new,
                "footage": all.iter().map(|c| c.duration).sum::<f64>(),
                "sync_seconds": self.last_sync_secs,
            },
        })
    }

    /// Status line after a finished sync.
    pub fn sync_summary(&self) -> (String, &'static str) {
        let all: Vec<&Clip> = self.tracks.iter().flat_map(|t| &t.clips).collect();
        let ok = all.iter().filter(|c| c.synced).count();
        let failed = all.len() - ok;
        // The status bar's counters carry the detail; no extra sentence needed.
        let _ = ok;
        (String::new(), if failed == 0 { "success" } else { "warning" })
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

    #[test]
    fn simultaneous_clips_get_separate_lanes() {
        let mut t = Track::new("Camera 1".into(), String::new(), true, String::new());
        t.clips.push(clip(1000.0, 120.0));
        assert!(!lane_fits(&t, &clip(1010.0, 115.0)));
    }
}
