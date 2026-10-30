//! Deterministic export for parity tests: export_fixture fixture.json resolve|fcpxml
use shotsynq_core::{export, model::{Clip, Track}};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let mut tracks: Vec<Track> = v.as_array().unwrap().iter().map(|t| {
        let mut tr = Track::new(t["display_name"].as_str().unwrap().into(), String::new(), t["is_video"].as_bool().unwrap(), String::new());
        tr.synced = t["synced"].as_bool().unwrap();
        tr.clips = t["clips"].as_array().unwrap().iter().map(|c| Clip {
            path: c["path"].as_str().unwrap().into(), name: c["name"].as_str().unwrap().into(),
            duration: c["duration"].as_f64().unwrap(), frame_rate: c["frame_rate"].as_f64(),
            timecode: c["timecode"].as_str().unwrap().into(), creation_time: None,
            width: c["width"].as_u64().unwrap() as u32, height: c["height"].as_u64().unwrap() as u32,
            is_video: c["is_video"].as_bool().unwrap(), num_audio_channels: c["ch"].as_u64().unwrap() as u32,
            fingerprint: String::new(), device: None, sample_rate: 48000, audio_loaded: true, load_error: None, waveform: vec![], features: None,
            offset: c["offset"].as_f64().unwrap(), synced: c["synced"].as_bool().unwrap(), confidence: 1.0,
        }).collect();
        tr
    }).collect();
    export::move_unsynced_to_end(&mut tracks);
    let strip = if a.get(3).is_some_and(|x| x == "strip") { export::scratch_to_strip(&tracks) } else { Default::default() };
    let mut n = 0;
    let mut ids = || { n += 1; format!("ID{n:04}") };
    let xml = if a[2] == "fcpxml" { export::build_fcpxml(&tracks, "SyncApp Project", &mut ids, &strip) } else { export::build_fcp7_xml(&tracks, "SyncApp Project", &mut ids, &strip) };
    print!("{}", xml.unwrap());
}
