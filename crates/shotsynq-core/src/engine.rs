//! Audio sync engine. Port of sync_engine.py (v2).
//!
//! 1. Features: 6 bands of log energy at 100 fps, normalised over a sliding
//!    1 s window; first/last 0.5 s ignored (record clicks).
//! 2. Every clip is cross-correlated (FFT) with every clip on other tracks.
//!    Each lag is scored as a z-statistic; confidence = peak-to-sidelobe ratio.
//! 3. Links accepted strongest-first; rejected if two clips from one track
//!    would overlap.
//! 4. Weighted least squares over all consistent links gives positions.
//! 5. Separate takes are laid back to back; unmatched clips go to the end.

use crate::model::{natural_cmp, Features};
use realfft::RealFftPlanner;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

// ── Tunables (same values as the Python version) ─────────────────────────────

pub const FPS: f64 = 100.0;
pub const N_FFT: usize = 512;
pub const BANDS_HZ: [(f64, f64); 6] =
    [(100.0, 300.0), (300.0, 700.0), (700.0, 1500.0), (1500.0, 3000.0), (3000.0, 6000.0), (6000.0, 8000.0)];
pub const DETREND_S: f64 = 1.0;
pub const EDGE_IGNORE_S: f64 = 0.5;
pub const MIN_OVERLAP_S: f64 = 4.0;
pub const MIN_PSR: f64 = 8.0;
pub const MIN_PEAK_RATIO: f64 = 1.25;
pub const PEAK_EXCLUDE_S: f64 = 0.5;
pub const SAME_TRACK_TOL_S: f64 = 0.5;
pub const CONSISTENCY_TOL_S: f64 = 0.08;
pub const SESSION_GAP_S: f64 = 0.0;
pub const PSR_FOR_FULL_CONF: f64 = 25.0;

// ── Features ─────────────────────────────────────────────────────────────────

fn hanning(n: usize) -> Vec<f64> {
    // numpy.hanning: symmetric
    if n == 1 {
        return vec![1.0];
    }
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()).collect()
}

/// numpy.searchsorted(freqs, x) with side='left'
fn searchsorted(freqs: &[f64], x: f64) -> usize {
    freqs.partition_point(|&f| f < x)
}

/// Centred moving mean with edge replication, identical to sync_engine._moving_mean.
fn moving_mean(x: &[f64], win: usize) -> Vec<f64> {
    let n = x.len();
    if n == 0 {
        return vec![];
    }
    if win <= 1 || n < win {
        let m = x.iter().sum::<f64>() / n as f64;
        return vec![m; n];
    }
    let left = win / 2 + 1;
    let right = win - win / 2 - 1;
    let mut padded = Vec::with_capacity(n + left + right);
    padded.extend(std::iter::repeat_n(x[0], left));
    padded.extend_from_slice(x);
    padded.extend(std::iter::repeat_n(x[n - 1], right));
    let mut c = Vec::with_capacity(padded.len());
    let mut acc = 0.0;
    for v in &padded {
        acc += v;
        c.push(acc);
    }
    (0..n).map(|i| (c[i + win] - c[i]) / win as f64).collect()
}

/// Mono audio at `sr` -> normalised band features. None if under 1 s.
pub fn compute_features(audio: &[f32], sr: u32) -> Option<Features> {
    let srf = sr as f64;
    if audio.len() < sr as usize {
        return None;
    }
    let hop = (srf / FPS).round() as usize;
    let window = hanning(N_FFT);
    let freqs: Vec<f64> = (0..=N_FFT / 2).map(|k| k as f64 * srf / N_FFT as f64).collect();
    let bands: Vec<(usize, usize)> = BANDS_HZ
        .iter()
        .filter(|(lo, _)| *lo < srf / 2.0)
        .map(|&(lo, hi)| {
            let a = searchsorted(&freqs, lo);
            let b = searchsorted(&freqs, hi.min(srf / 2.0));
            (a, b.max(a + 1))
        })
        .collect();
    let nb = bands.len();

    // Frames over zero-padded signal (pad N_FFT/2 each side), hop `hop`.
    let half = N_FFT / 2;
    let padded_len = audio.len() + N_FFT;
    let n_windows = padded_len - N_FFT + 1;
    let n_frames = n_windows.div_ceil(hop);

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let mut inbuf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut energy = vec![0.0f64; nb * n_frames];

    for f in 0..n_frames {
        let start = f * hop; // index into padded signal
        for (i, slot) in inbuf.iter_mut().enumerate() {
            let p = start + i;
            let s = if p < half || p - half >= audio.len() { 0.0 } else { audio[p - half] as f64 };
            *slot = s * window[i];
        }
        fft.process_with_scratch(&mut inbuf, &mut spec, &mut scratch).ok()?;
        for (b, &(lo, hi)) in bands.iter().enumerate() {
            let mut e = 0.0;
            for c in &spec[lo..hi.min(spec.len())] {
                e += c.re * c.re + c.im * c.im;
            }
            energy[b * n_frames + f] = e;
        }
    }

    let win = (DETREND_S * FPS) as usize;
    let edge = (EDGE_IGNORE_S * FPS) as usize;
    let mut data = vec![0.0f32; nb * n_frames];
    for b in 0..nb {
        let row: Vec<f64> = energy[b * n_frames..(b + 1) * n_frames].iter().map(|e| (e + 1e-9).ln()).collect();
        let mm = moving_mean(&row, win);
        let det: Vec<f64> = row.iter().zip(&mm).map(|(a, m)| a - m).collect();
        let mean = det.iter().sum::<f64>() / n_frames as f64;
        let var = det.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n_frames as f64;
        let mut sd = var.sqrt();
        if sd < 1e-9 {
            sd = 1.0;
        }
        for (i, v) in det.iter().enumerate() {
            data[b * n_frames + i] = (v / sd) as f32;
        }
        if n_frames > 4 * edge {
            for i in (0..edge).chain(n_frames - edge..n_frames) {
                data[b * n_frames + i] = 0.0;
            }
        }
    }
    Some(Features { bands: nb, frames: n_frames, data })
}

// ── Pairwise matching ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct PairMatch {
    /// pos_B - pos_A in seconds
    pub offset: f64,
    pub psr: f64,
    pub ratio: f64,
}

/// Where clip B starts relative to clip A.
pub fn match_pair(fa: &Features, fb: &Features, planner: &mut RealFftPlanner<f64>) -> Option<PairMatch> {
    let (la, lb) = (fa.frames, fb.frames);
    let min_ov = ((MIN_OVERLAP_S * FPS).min(0.8 * la.min(lb) as f64)) as i64;
    if (min_ov as f64) < FPS {
        return None;
    }
    let n = (la + lb).next_power_of_two();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let nb = fa.bands.min(fb.bands);

    let mut acc = fwd.make_output_vec(); // summed cross-spectrum
    let mut buf = fwd.make_input_vec();
    let mut sa = fwd.make_output_vec();
    let mut sb = fwd.make_output_vec();
    let mut scratch = fwd.make_scratch_vec();
    for b in 0..nb {
        buf.iter_mut().for_each(|v| *v = 0.0);
        for (d, s) in buf.iter_mut().zip(fa.band(b)) {
            *d = *s as f64;
        }
        fwd.process_with_scratch(&mut buf, &mut sa, &mut scratch).ok()?;
        buf.iter_mut().for_each(|v| *v = 0.0);
        for (d, s) in buf.iter_mut().zip(fb.band(b)) {
            *d = *s as f64;
        }
        fwd.process_with_scratch(&mut buf, &mut sb, &mut scratch).ok()?;
        for k in 0..acc.len() {
            acc[k] += sa[k] * sb[k].conj();
        }
    }
    // realfft requires purely real DC/Nyquist bins on inverse
    acc[0].im = 0.0;
    let last = acc.len() - 1;
    acc[last].im = 0.0;
    let mut xc = inv.make_output_vec();
    let mut iscratch = inv.make_scratch_vec();
    inv.process_with_scratch(&mut acc, &mut xc, &mut iscratch).ok()?;
    let scale = 1.0 / n as f64; // realfft is unnormalised

    // lags -(lb-1) ..= la-1; c[k] = sum_t a[t+k] b[t]
    let mut lags = vec![];
    let mut z = vec![];
    for k in -(lb as i64 - 1)..(la as i64) {
        let overlap = (la as i64).min(k + lb as i64) - k.max(0);
        if overlap < min_ov {
            continue;
        }
        let idx = k.rem_euclid(n as i64) as usize;
        let val = xc[idx] * scale;
        lags.push(k);
        z.push(val / ((overlap as f64) * fa.bands as f64).sqrt());
    }
    if (z.len() as f64) < 3.0 * FPS {
        return None;
    }

    let p = z
        .iter()
        .enumerate()
        .fold((0, f64::NEG_INFINITY), |m, (i, &v)| if v > m.1 { (i, v) } else { m })
        .0;
    let excl = (PEAK_EXCLUDE_S * FPS) as usize;
    let lo = p.saturating_sub(excl);
    let hi = (p + excl + 1).min(z.len());
    let noise: Vec<f64> = z[..lo].iter().chain(&z[hi..]).copied().collect();
    if (noise.len() as f64) < FPS {
        return None;
    }
    let mu = noise.iter().sum::<f64>() / noise.len() as f64;
    let sd = (noise.iter().map(|v| (v - mu) * (v - mu)).sum::<f64>() / noise.len() as f64).sqrt() + 1e-9;
    let psr = (z[p] - mu) / sd;
    let second = noise.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let ratio = (z[p] - mu) / (second - mu).max(1e-9);

    let mut frac = 0.0;
    if p > 0 && p < z.len() - 1 {
        let (y0, y1, y2) = (z[p - 1], z[p], z[p + 1]);
        let d = y0 - 2.0 * y1 + y2;
        if d < 0.0 {
            frac = 0.5 * (y0 - y2) / d;
        }
    }
    Some(PairMatch { offset: (lags[p] as f64 + frac) / FPS, psr, ratio })
}

// ── Engine I/O ───────────────────────────────────────────────────────────────

pub struct SyncInput {
    pub name: String,
    pub track: usize,
    pub duration: f64,
    pub creation_time: Option<String>,
    pub features: Option<Arc<Features>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub offset: f64,
    pub synced: bool,
    pub confidence: f64,
}

#[derive(Debug)]
pub enum SyncError {
    Cancelled,
    NeedTwoTracks,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Cancelled => write!(f, "Cancelled"),
            SyncError::NeedTwoTracks => write!(f, "Need at least 2 tracks to sync."),
        }
    }
}

// ── Grouping ─────────────────────────────────────────────────────────────────

struct Groups {
    parent: Vec<usize>,
    pos: Vec<f64>,
    members: HashMap<usize, Vec<usize>>,
}

impl Groups {
    fn new(n: usize) -> Self {
        Groups { parent: (0..n).collect(), pos: vec![0.0; n], members: (0..n).map(|i| (i, vec![i])).collect() }
    }
    fn find(&self, mut i: usize) -> usize {
        while self.parent[i] != i {
            i = self.parent[i];
        }
        i
    }
    fn merge(&mut self, ra: usize, rb: usize, shift: f64) {
        let moved = self.members.remove(&rb).unwrap_or_default();
        for &m in &moved {
            self.pos[m] += shift;
            self.parent[m] = ra;
        }
        self.parent[rb] = ra;
        self.members.get_mut(&ra).unwrap().extend(moved);
    }
}

fn same_track_conflict(g: &Groups, ra: usize, rb: usize, shift: f64, inputs: &[SyncInput]) -> bool {
    let mut by_track: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
    for &m in &g.members[&ra] {
        by_track.entry(inputs[m].track).or_default().push((g.pos[m], inputs[m].duration));
    }
    for &m in &g.members[&rb] {
        if let Some(list) = by_track.get(&inputs[m].track) {
            let s = g.pos[m] + shift;
            let e = s + inputs[m].duration;
            for &(os, od) in list {
                if e.min(os + od) - s.max(os) > SAME_TRACK_TOL_S {
                    return true;
                }
            }
        }
    }
    false
}

/// Weighted least squares with first member anchored (normal equations).
fn least_squares(members: &[usize], edges: &[(usize, usize, f64, f64)], rough: &[f64]) -> HashMap<usize, f64> {
    let idx: HashMap<usize, usize> = members.iter().enumerate().map(|(k, &m)| (m, k)).collect();
    let m = members.len();
    let mut rows: Vec<(usize, usize, f64, f64)> = vec![];
    for &(i, j, d, psr) in edges {
        if let (Some(&a), Some(&b)) = (idx.get(&i), idx.get(&j)) {
            if ((rough[j] - rough[i]) - d).abs() <= CONSISTENCY_TOL_S {
                rows.push((a, b, d, psr));
            }
        }
    }
    if rows.is_empty() {
        return members.iter().map(|&k| (k, rough[k])).collect();
    }
    let wmax = rows.iter().map(|r| r.3).fold(0.0, f64::max);
    let mut ata = vec![vec![0.0; m]; m];
    let mut atb = vec![0.0; m];
    for &(a, b, d, w) in &rows {
        // row: -x_a + x_b = d, weight w
        ata[a][a] += w;
        ata[b][b] += w;
        ata[a][b] -= w;
        ata[b][a] -= w;
        atb[a] -= w * d;
        atb[b] += w * d;
    }
    let wa = wmax * 10.0;
    ata[0][0] += wa;
    atb[0] += wa * rough[members[0]];
    let sol = solve(ata, atb);
    members.iter().enumerate().map(|(k, &mm)| (mm, sol[k])).collect()
}

/// Gaussian elimination with partial pivoting.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&x, &y| a[x][col].abs().partial_cmp(&a[y][col].abs()).unwrap()).unwrap();
        a.swap(col, piv);
        b.swap(col, piv);
        let p = a[col][col];
        if p.abs() < 1e-12 {
            continue;
        }
        for r in col + 1..n {
            let f = a[r][col] / p;
            if f == 0.0 {
                continue;
            }
            for c in col..n {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let mut s = b[r];
        for c in r + 1..n {
            s -= a[r][c] * x[c];
        }
        x[r] = if a[r][r].abs() < 1e-12 { 0.0 } else { s / a[r][r] };
    }
    x
}

// ── Session ordering ─────────────────────────────────────────────────────────

/// Parse ffprobe creation_time ("2024-04-17T13:47:43.000000Z" and similar)
/// into seconds. Only differences between values matter.
pub fn parse_creation_time(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once(['T', ' '])?;
    let d: Vec<i64> = date.split(['-', '/']).map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if d.len() != 3 {
        return None;
    }
    let time = time.split(['+']).next()?;
    let t: Vec<&str> = time.split(':').collect();
    if t.len() != 3 {
        return None;
    }
    let (h, mi): (i64, i64) = (t[0].parse().ok()?, t[1].parse().ok()?);
    let sec: f64 = t[2].parse().ok()?;
    // days from civil (Howard Hinnant)
    let (y, m, dd) = (d[0], d[1], d[2]);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days as f64 * 86400.0 + (h * 3600 + mi * 60) as f64 + sec)
}

struct SessionInfo {
    clock: HashMap<usize, f64>,
    rank: HashMap<usize, usize>,
}

fn order_sessions(sessions: &mut Vec<(Vec<usize>, HashMap<usize, f64>)>, inputs: &[SyncInput]) {
    let mut name_rank = vec![0usize; inputs.len()];
    let tracks: std::collections::BTreeSet<usize> = inputs.iter().map(|c| c.track).collect();
    for t in tracks {
        let mut members: Vec<usize> = (0..inputs.len()).filter(|&i| inputs[i].track == t).collect();
        members.sort_by(|&a, &b| natural_cmp(&inputs[a].name, &inputs[b].name));
        for (r, i) in members.into_iter().enumerate() {
            name_rank[i] = r;
        }
    }
    let info: Vec<SessionInfo> = sessions
        .iter()
        .map(|(members, pos)| {
            let mut clock: HashMap<usize, Vec<f64>> = HashMap::new();
            let mut rank: HashMap<usize, usize> = HashMap::new();
            for &m in members {
                let t = inputs[m].track;
                if let Some(ts) = inputs[m].creation_time.as_deref().and_then(parse_creation_time) {
                    clock.entry(t).or_default().push(ts - pos[&m]);
                }
                let r = rank.entry(t).or_insert(usize::MAX);
                *r = (*r).min(name_rank[m]);
            }
            let clock = clock.into_iter().map(|(t, mut v)| (t, median(&mut v))).collect();
            SessionInfo { clock, rank }
        })
        .collect();

    let cmp = |x: usize, y: usize| -> Ordering {
        let (a, b) = (&info[x], &info[y]);
        let mut shared: Vec<&usize> = a.clock.keys().filter(|t| b.clock.contains_key(t)).collect();
        shared.sort();
        for t in shared {
            let (ca, cb) = (a.clock[t], b.clock[t]);
            if (ca - cb).abs() > 1.0 {
                return ca.partial_cmp(&cb).unwrap_or(Ordering::Equal);
            }
        }
        let mut shared: Vec<&usize> = a.rank.keys().filter(|t| b.rank.contains_key(t)).collect();
        shared.sort();
        for t in shared {
            if a.rank[t] != b.rank[t] {
                return a.rank[t].cmp(&b.rank[t]);
            }
        }
        let ma = a.rank.values().min().copied().unwrap_or(0);
        let mb = b.rank.values().min().copied().unwrap_or(0);
        ma.cmp(&mb)
    };

    // Insertion sort: the comparator may not be a strict total order, and
    // std's sort can panic on that.
    let mut order: Vec<usize> = (0..sessions.len()).collect();
    for i in 1..order.len() {
        let mut j = i;
        while j > 0 && cmp(order[j - 1], order[j]) == Ordering::Greater {
            order.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut taken: Vec<Option<(Vec<usize>, HashMap<usize, f64>)>> = sessions.drain(..).map(Some).collect();
    for i in order {
        sessions.push(taken[i].take().unwrap());
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

// ── Main ─────────────────────────────────────────────────────────────────────

pub fn sync(
    inputs: &[SyncInput],
    progress: &dyn Fn(String),
    cancel: &AtomicBool,
) -> Result<Vec<Placement>, SyncError> {
    let n_tracks = inputs.iter().map(|c| c.track).collect::<std::collections::HashSet<_>>().len();
    if n_tracks < 2 {
        return Err(SyncError::NeedTwoTracks);
    }
    let n = inputs.len();
    let check = || if cancel.load(AtomicOrdering::Relaxed) { Err(SyncError::Cancelled) } else { Ok(()) };

    // 2. Pairwise matches across tracks (parallel)
    let pairs: Vec<(usize, usize)> = (0..n)
        .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
        .filter(|&(i, j)| inputs[i].track != inputs[j].track && inputs[i].features.is_some() && inputs[j].features.is_some())
        .collect();
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).clamp(1, 16);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, usize, PairMatch)>> = std::sync::Mutex::new(vec![]);
    let total = pairs.len().max(1);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut planner = RealFftPlanner::<f64>::new();
                loop {
                    if cancel.load(AtomicOrdering::Relaxed) {
                        break;
                    }
                    let k = next.fetch_add(1, AtomicOrdering::Relaxed);
                    if k >= pairs.len() {
                        break;
                    }
                    let (i, j) = pairs[k];
                    let fa = inputs[i].features.as_ref().unwrap();
                    let fb = inputs[j].features.as_ref().unwrap();
                    if let Some(m) = match_pair(fa, fb, &mut planner) {
                        results.lock().unwrap().push((i, j, m));
                    }
                    done.fetch_add(1, AtomicOrdering::Relaxed);
                }
            });
        }
        let mut last = usize::MAX;
        loop {
            let d = done.load(AtomicOrdering::Relaxed);
            let pct = d * 100 / total;
            if pct != last {
                progress(format!("Matching clips... {pct}%"));
                last = pct;
            }
            if d >= pairs.len() || cancel.load(AtomicOrdering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    check()?;

    let mut matches = results.into_inner().unwrap();
    matches.sort_by_key(|&(i, j, _)| (i, j)); // deterministic
    let mut edges: Vec<(usize, usize, f64, f64)> = vec![];
    for (i, j, m) in matches {
        if m.psr >= MIN_PSR && m.ratio >= MIN_PEAK_RATIO {
            edges.push((i, j, m.offset, m.psr));
            log::info!(
                "link {} -> {}: {:+.3}s psr={:.1} ratio={:.2}",
                inputs[i].name, inputs[j].name, m.offset, m.psr, m.ratio
            );
        }
    }

    // 3. Groups, strongest first
    progress("Building timeline...".into());
    let mut g = Groups::new(n);
    let mut best_psr = vec![0.0f64; n];
    let mut sorted = edges.clone();
    sorted.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(Ordering::Equal));
    for &(i, j, d, psr) in &sorted {
        let (mut ri, mut rj) = (g.find(i), g.find(j));
        if ri == rj {
            continue;
        }
        let mut shift = g.pos[i] + d - g.pos[j];
        if same_track_conflict(&g, ri, rj, shift, inputs) {
            log::info!("rejected {} -> {} (same-track overlap)", inputs[i].name, inputs[j].name);
            continue;
        }
        if g.members[&ri].len() < g.members[&rj].len() {
            std::mem::swap(&mut ri, &mut rj);
            shift = -shift;
        }
        g.merge(ri, rj, shift);
        best_psr[i] = best_psr[i].max(psr);
        best_psr[j] = best_psr[j].max(psr);
    }
    let rough = g.pos.clone();
    for &(i, j, d, psr) in &edges {
        if g.find(i) == g.find(j) && ((rough[j] - rough[i]) - d).abs() <= CONSISTENCY_TOL_S {
            best_psr[i] = best_psr[i].max(psr);
            best_psr[j] = best_psr[j].max(psr);
        }
    }

    // 4./5. Solve and lay out
    let mut roots: Vec<usize> = g.members.keys().copied().collect();
    roots.sort();
    let mut sessions: Vec<(Vec<usize>, HashMap<usize, f64>)> = vec![];
    for r in roots {
        let mut members = g.members[&r].clone();
        if members.len() < 2 {
            continue;
        }
        members.sort();
        // Python anchors on the group's first-joined member; any anchor gives
        // the same relative positions.
        let pos = least_squares(&members, &edges, &rough);
        let start = members.iter().map(|m| pos[m]).fold(f64::INFINITY, f64::min);
        let pos: HashMap<usize, f64> = pos.into_iter().map(|(k, v)| (k, v - start)).collect();
        sessions.push((members, pos));
    }
    order_sessions(&mut sessions, inputs);

    let mut out = vec![Placement { offset: 0.0, synced: false, confidence: 0.0 }; n];
    let mut cursor = 0.0;
    for (s_idx, (members, pos)) in sessions.iter().enumerate() {
        let mut end: f64 = 0.0;
        for &m in members {
            let off = round4(cursor + pos[&m]);
            out[m] = Placement { offset: off, synced: true, confidence: round4((best_psr[m] / PSR_FOR_FULL_CONF).min(1.0)) };
            end = end.max(off + inputs[m].duration);
        }
        log::info!("Session {}: {} clips, {:.2}s -> {:.2}s", s_idx + 1, members.len(), cursor, end);
        cursor = end + SESSION_GAP_S;
    }
    let tail = if sessions.is_empty() { 0.0 } else { cursor - SESSION_GAP_S + SESSION_GAP_S };

    // Unsynced: after everything, per track, in name order
    let mut track_ids: Vec<usize> = inputs.iter().map(|c| c.track).collect();
    track_ids.sort();
    track_ids.dedup();
    for t in track_ids {
        let mut un: Vec<usize> = (0..n).filter(|&i| inputs[i].track == t && !out[i].synced).collect();
        un.sort_by(|&a, &b| natural_cmp(&inputs[a].name, &inputs[b].name));
        let mut run = tail;
        for i in un {
            out[i].offset = round4(run);
            run += inputs[i].duration;
            log::info!("UNSYNCED {}", inputs[i].name);
        }
    }
    let synced = out.iter().filter(|p| p.synced).count();
    progress(format!("Done: {synced}/{n} clips synced in {} session(s)", sessions.len()));
    Ok(out)
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round_ties_even() / 10000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_time_parses() {
        let a = parse_creation_time("2024-04-17T13:47:43.000000Z").unwrap();
        let b = parse_creation_time("2024-04-17T13:48:43Z").unwrap();
        assert_eq!(b - a, 60.0);
        assert_eq!(parse_creation_time("1970-01-01 00:00:00"), Some(0.0));
        assert!(parse_creation_time("garbage").is_none());
    }

    #[test]
    fn moving_mean_matches_python() {
        let x: Vec<f64> = (0..10).map(|v| v as f64).collect();
        // python: _moving_mean(np.arange(10.), 4)
        let r = moving_mean(&x, 4);
        let expect = [0.25, 0.75, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.25];
        for (a, b) in r.iter().zip(expect) {
            assert!((a - b).abs() < 1e-12, "{r:?}");
        }
    }
}
