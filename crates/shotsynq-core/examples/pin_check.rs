use std::sync::atomic::AtomicBool;
use shotsynq_core::{engine, media, session::Session};
fn sync(s: &mut Session) {
    let (inputs, map) = s.sync_inputs();
    let paths: Vec<String> = map.iter().map(|&(t, c)| s.tracks[t].clips[c].path.clone()).collect();
    let res = engine::sync(&inputs, &|_| {}, &AtomicBool::new(false)).unwrap();
    s.apply_placements(&map, &paths, &res);
    s.synced = true;
}
fn show(s: &Session, l: &str) {
    println!("-- {l}");
    for t in &s.tracks { for c in &t.clips { println!("  {:24} {:12} off {:8.3} synced {} manual {}", c.name, t.display_name, c.offset, c.synced, c.manual); } }
}
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut s = Session::new();
    let (b, paths) = s.begin_batch(a.clone());
    for p in &paths { let mut c = media::load_metadata(p).unwrap(); s.add_clip(b, c.clone()); media::load_audio(&mut c).unwrap(); s.set_audio(p, c.waveform, c.features, c.audio_note, None); }
    sync(&mut s);
    show(&s, "first sync");
    let (ti, ci) = s.tracks.iter().enumerate().find_map(|(ti, t)| t.clips.iter().position(|c| !c.synced).map(|ci| (ti, ci))).unwrap();
    let cam_off = s.tracks[0].clips[0].offset;
    s.move_clip(ti, ci, cam_off + 92.0, false);
    show(&s, "moved unsynced clip to camera + 92 s");
    sync(&mut s);
    show(&s, "re-sync");
    let m = s.tracks[ti].clips[ci].offset - s.tracks[0].clips[0].offset;
    println!("manual clip vs camera after re-sync: {m:.3} s (expected 92)");
}
