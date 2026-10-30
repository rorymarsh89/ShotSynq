//! ShotSynq CLI: cargo run --example cli -- tracks.json [--export resolve|premiere|fcpxml out.xml]
//! tracks.json: [{"name": "Camera 1", "paths": ["a.mxf", ...]}, ...]
use std::sync::atomic::AtomicBool;
use shotsynq_core::{engine, export, media, model::Track};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let spec: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let mut tracks = vec![];
    for t in spec.as_array().unwrap() {
        let paths: Vec<String> = t["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
        let mut clips: Vec<_> = std::thread::scope(|s| {
            let hs: Vec<_> = paths.iter().map(|p| s.spawn(move || {
                let mut c = media::load_metadata(p).unwrap();
                media::load_audio(&mut c).unwrap();
                c
            })).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        clips.sort_by(|a, b| shotsynq_core::model::natural_cmp(&a.name, &b.name));
        let first = &clips[0];
        let mut tr = Track::new(t["name"].as_str().unwrap().into(), first.fingerprint.clone(), first.is_video, "#4f8ef7".into());
        tr.clips = clips;
        tracks.push(tr);
    }
    let mut inputs = vec![];
    let mut map = vec![];
    for (ti, t) in tracks.iter().enumerate() {
        for (ci, c) in t.clips.iter().enumerate() {
            inputs.push(engine::SyncInput { name: c.name.clone(), track: ti, duration: c.duration,
                creation_time: c.creation_time.clone(), features: c.features.clone() });
            map.push((ti, ci));
        }
    }
    let t0 = std::time::Instant::now();
    let res = engine::sync(&inputs, &|_m| {}, &AtomicBool::new(false)).unwrap();
    eprintln!("sync took {:.2}s", t0.elapsed().as_secs_f64());
    let mut out = serde_json::Map::new();
    for (&(ti, ci), p) in map.iter().zip(&res) {
        let c = &mut tracks[ti].clips[ci];
        c.offset = p.offset; c.synced = p.synced; c.confidence = p.confidence;
        out.insert(c.name.clone(), serde_json::json!({"offset": p.offset, "synced": p.synced, "confidence": p.confidence}));
    }
    for t in &mut tracks { t.synced = t.clips.iter().any(|c| c.synced); }
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    if args.len() >= 4 && args[2] == "--export" {
        let fmt = match args[3].as_str() { "fcpxml" => export::Format::Fcpxml, "premiere" => export::Format::Premiere, _ => export::Format::Resolve };
        export::export(&tracks, fmt, &args[4], export::ExportOptions::default()).unwrap();
    }
}
