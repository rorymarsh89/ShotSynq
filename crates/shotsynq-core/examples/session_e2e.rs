//! End-to-end check: load files into a Session (as the app does), sync, export.
//! session_e2e out.xml file1 file2 ...
use std::sync::atomic::AtomicBool;
use shotsynq_core::{engine, export, media, session::Session};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut s = Session::new();
    let (batch, paths) = s.begin_batch(a[2..].to_vec());
    for p in &paths {
        let mut c = media::load_metadata(p).unwrap();
        s.add_clip(batch, c.clone());
        media::load_audio(&mut c).unwrap();
        s.set_audio(p, c.waveform, c.features, c.audio_note, None);
    }
    let (inputs, map) = s.sync_inputs();
    let paths: Vec<String> = map.iter().map(|&(t, c)| s.tracks[t].clips[c].path.clone()).collect();
    let res = engine::sync(&inputs, &|_| {}, &AtomicBool::new(false)).unwrap();
    s.apply_placements(&map, &paths, &res);
    s.synced = true;
    for t in &s.tracks {
        println!("[{}] meta=\"{}\"", t.display_name, t.meta_lines().join(" / "));
        for c in &t.clips {
            println!("    {:24} offset={:8.3} synced={} conf={:.2}", c.name, c.offset, c.synced, c.confidence);
        }
    }
    export::export(&s.tracks, export::Format::Resolve, &a[1], export::ExportOptions { strip_camera_audio: true, drop_unsynced: false }).unwrap();
}
