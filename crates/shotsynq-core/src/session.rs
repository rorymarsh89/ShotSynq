//! App state: tracks, grouping files into tracks, applying sync results and
//! building the JSON the UI renders. Port of api.py (minus the GUI plumbing).

use crate::engine::{Placement, SyncInput};
use crate::model::{natural_cmp, Clip, Features, Track, TRACK_COLORS};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CONFIDENCE_OK: f64 = 0.5;

/// 1 -> "A", 26 -> "Z", 27 -> "AA"
fn letter(mut n: usize) -> String {
    let mut s = String::new();
    while n > 0 {
        n -= 1;
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    s
}

#[derive(Default)]
pub struct Session {
    pub tracks: Vec<Track>,
    camera_counter: usize,
    audio_counter: usize,
    /// (batch id, kind, fingerprint) -> track index
    batch_tracks: HashMap<(u64, bool, String), usize>,
    pending: HashSet<String>,
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
            .filter(|p| !existing.contains(p.as_str()) && !self.pending.contains(p) && seen.insert(p.clone()))
            .collect();
        for p in &new {
            self.pending.insert(p.clone());
        }
        self.next_batch += 1;
        (self.next_batch, new)
    }

    pub fn load_failed(&mut self, path: &str) {
        self.pending.remove(path);
    }

    /// Metadata is ready: put the clip in the matching track for this batch.
    pub fn add_clip(&mut self, batch: u64, clip: Clip) {
        self.pending.remove(&clip.path);
        let key = (batch, clip.is_video, clip.fingerprint.clone());
        let ti = match self.batch_tracks.get(&key) {
            Some(&ti) if ti < self.tracks.len() => ti,
            _ => {
                let name = if clip.is_video {
                    self.camera_counter += 1;
                    format!("Camera {}", letter(self.camera_counter))
                } else {
                    self.audio_counter += 1;
                    if self.audio_counter == 1 { "Recorder".to_string() } else { format!("Recorder {}", self.audio_counter) }
                };
                let color = TRACK_COLORS[self.tracks.len() % TRACK_COLORS.len()].to_string();
                // Cameras first, then recorders, whatever order files finish loading in
                let ti = if clip.is_video {
                    self.tracks.iter().take_while(|t| t.is_video).count()
                } else {
                    self.tracks.len()
                };
                self.tracks.insert(ti, Track::new(name, clip.fingerprint.clone(), clip.is_video, color));
                self.reindex_batches(|i| Some(if i >= ti { i + 1 } else { i }));
                self.batch_tracks.insert(key, ti);
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
        error: Option<String>,
    ) {
        for c in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if c.path == path {
                c.waveform = waveform;
                c.features = features;
                c.load_error = error;
                c.audio_loaded = true;
                return;
            }
        }
    }

    pub fn remove_track(&mut self, i: usize) {
        if i < self.tracks.len() {
            self.tracks.remove(i);
            self.reindex_batches(|ti| if ti == i { None } else if ti > i { Some(ti - 1) } else { Some(ti) });
        }
    }

    fn reindex_batches(&mut self, f: impl Fn(usize) -> Option<usize>) {
        self.batch_tracks = self.batch_tracks.drain().filter_map(|(k, v)| f(v).map(|nv| (k, nv))).collect();
    }

    pub fn rename_track(&mut self, i: usize, name: String) {
        if let Some(t) = self.tracks.get_mut(i) {
            t.display_name = name;
        }
    }

    pub fn delete_clip(&mut self, ti: usize, ci: usize) {
        if let Some(t) = self.tracks.get_mut(ti) {
            if ci < t.clips.len() {
                t.clips.remove(ci);
            }
        }
    }

    /// Remove every clip that failed to sync (after a sync).
    pub fn remove_unsynced(&mut self) -> usize {
        if !self.synced {
            return 0;
        }
        let before: usize = self.tracks.iter().map(|t| t.clips.len()).sum();
        for t in &mut self.tracks {
            t.clips.retain(|c| c.synced);
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
        // After a sync, unplaced clips live in the tray, not on the timeline.
        let max_end = if total == 0 {
            0.0
        } else if any_synced {
            all.iter().filter(|c| c.synced).map(|c| c.offset + c.duration).fold(0.0, f64::max)
        } else {
            self.tracks.iter().map(|t| t.clips.iter().map(|c| c.duration).sum::<f64>()).fold(0.0, f64::max)
        };
        let pending = self.pending.len();
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
                            "status": self.clip_status(c),
                            "confidence": c.confidence,
                            "waveform": c.waveform,
                            "frame_rate": c.frame_rate,
                            "width": c.width,
                            "height": c.height,
                            "creation_time": c.creation_time.clone().unwrap_or_default(),
                            "timecode": c.timecode,
                            "error": c.load_error,
                        })
                    })
                    .collect();
                json!({
                    "index": i,
                    "display_name": t.display_name,
                    "color": t.color,
                    "is_video": t.is_video,
                    "meta": t.meta_string(),
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
