// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use shotsynq_core::{engine, export, media, session::Session};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

const MEDIA_EXTS: [&str; 11] = ["mp4", "mov", "mxf", "avi", "mkv", "mts", "mp3", "wav", "aac", "m4a", "bwf"];

struct Job {
    generation: u64,
    batch: u64,
    path: String,
}

struct AppState {
    session: Mutex<Session>,
    generation: Mutex<u64>,
    cancel: Mutex<Arc<AtomicBool>>,
    jobs: Mutex<Sender<Job>>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn push_state(app: &AppHandle) {
    let st = app.state::<AppState>();
    let state = st.session.lock().unwrap().state();
    let _ = app.emit("state", state);
}

fn push_state_with_status(app: &AppHandle, text: &str, kind: &str) {
    let st = app.state::<AppState>();
    let mut state = st.session.lock().unwrap().state();
    state["status"] = json!({"text": text, "type": kind});
    let _ = app.emit("state", state);
}

fn set_status(app: &AppHandle, text: &str, kind: &str) {
    let _ = app.emit("status", json!({"text": text, "type": kind}));
}

fn is_media(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Files as-is; folders expanded to the media inside them (a few levels deep).
fn expand_paths(paths: Vec<String>) -> Vec<String> {
    fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if depth < 4 {
                    walk(&p, depth + 1, out);
                }
            } else {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    let mut out = vec![];
    for p in paths {
        let path = std::path::Path::new(&p);
        if path.is_dir() {
            walk(path, 0, &mut out);
        } else {
            out.push(p);
        }
    }
    out
}

fn enqueue(app: &AppHandle, paths: Vec<String>) {
    let st = app.state::<AppState>();
    let given = paths.len();
    let paths: Vec<String> = expand_paths(paths).into_iter().filter(|p| is_media(p)).collect();
    if paths.is_empty() {
        if given > 0 {
            set_status(app, "No video or audio files found in what you dropped", "warning");
        }
        return;
    }
    if st.session.lock().unwrap().syncing {
        set_status(app, "Wait for the sync to finish before adding media", "warning");
        return;
    }
    let generation = *st.generation.lock().unwrap();
    let (batch, new) = st.session.lock().unwrap().begin_batch(paths);
    if new.is_empty() {
        set_status(app, "Those files are already added", "default");
        return;
    }
    let tx = st.jobs.lock().unwrap().clone();
    for path in new {
        let _ = tx.send(Job { generation, batch, path });
    }
    push_state(app);
}

/// Loader pool: FFprobe for metadata, then decode audio + build features.
fn start_loaders(app: AppHandle, rx: Receiver<Job>) {
    let rx = Arc::new(Mutex::new(rx));
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
    for _ in 0..n {
        let rx = rx.clone();
        let app = app.clone();
        std::thread::spawn(move || loop {
            let job = match rx.lock().unwrap().recv() {
                Ok(j) => j,
                Err(_) => return,
            };
            let st = app.state::<AppState>();
            let current = || *st.generation.lock().unwrap() == job.generation;
            if !current() {
                continue;
            }
            let clip = match media::load_metadata(&job.path) {
                Ok(c) => c,
                Err(e) => {
                    st.session.lock().unwrap().load_failed(&job.path);
                    let name = std::path::Path::new(&job.path).file_name().unwrap_or_default().to_string_lossy();
                    push_state(&app);
                    set_status(&app, &format!("Error loading {name}: {e}"), "error");
                    continue;
                }
            };
            if !current() {
                continue;
            }
            st.session.lock().unwrap().add_clip(job.batch, clip.clone());
            push_state(&app);

            let mut loaded = clip;
            let err = media::load_audio(&mut loaded).err();
            if !current() {
                continue;
            }
            st.session.lock().unwrap().set_audio(&job.path, loaded.waveform, loaded.features, err.clone());
            push_state(&app);
            if let Some(e) = err {
                set_status(&app, &format!("Couldn't read audio from {}: {e}", loaded.name), "error");
            }
        });
    }
}

// ── Commands ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_state(st: State<AppState>) -> Value {
    st.session.lock().unwrap().state()
}

#[tauri::command]
fn add_files(app: AppHandle) {
    let app2 = app.clone();
    app.dialog()
        .file()
        .set_title("Select media files")
        .add_filter("Media files", &MEDIA_EXTS)
        .add_filter("All files", &["*"])
        .pick_files(move |files| {
            if let Some(files) = files {
                let paths = files
                    .into_iter()
                    .filter_map(|f| f.into_path().ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                enqueue(&app2, paths);
            }
        });
}

#[tauri::command]
fn add_files_from_paths(app: AppHandle, paths: Vec<String>) {
    enqueue(&app, paths);
}

#[tauri::command]
fn run_sync(app: AppHandle, st: State<AppState>) {
    let (inputs, map, paths) = {
        let mut s = st.session.lock().unwrap();
        if !s.can_sync() {
            return;
        }
        s.syncing = true;
        s.synced = false;
        let (inputs, map) = s.sync_inputs();
        let paths: Vec<String> = map.iter().map(|&(t, c)| s.tracks[t].clips[c].path.clone()).collect();
        (inputs, map, paths)
    };
    let cancel = Arc::new(AtomicBool::new(false));
    *st.cancel.lock().unwrap() = cancel.clone();
    push_state_with_status(&app, "Synchronising…", "loading");

    std::thread::spawn(move || {
        let progress = |m: String| set_status(&app, &m, "loading");
        let t0 = std::time::Instant::now();
        let result = engine::sync(&inputs, &progress, &cancel);
        let took = t0.elapsed().as_secs_f64();
        let st = app.state::<AppState>();
        let mut s = st.session.lock().unwrap();
        s.syncing = false;
        match result {
            Ok(placements) => {
                if s.apply_placements(&map, &paths, &placements) {
                    s.synced = true;
                    s.last_sync_secs = Some(took);
                    let (text, kind) = s.sync_summary();
                    drop(s);
                    push_state_with_status(&app, &text, kind);
                } else {
                    drop(s);
                    push_state_with_status(&app, "Media changed during sync — please synchronise again", "warning");
                }
            }
            Err(engine::SyncError::Cancelled) => {
                drop(s);
                push_state_with_status(&app, "Sync cancelled", "default");
            }
            Err(e) => {
                drop(s);
                push_state_with_status(&app, &format!("Sync failed: {e}"), "error");
            }
        }
    });
}

#[tauri::command]
fn cancel_sync(st: State<AppState>) {
    st.cancel.lock().unwrap().store(true, Ordering::Relaxed);
}

#[tauri::command]
fn remove_track(app: AppHandle, st: State<AppState>, index: usize) {
    st.session.lock().unwrap().remove_track(index);
    push_state(&app);
}

#[tauri::command]
fn rename_track(st: State<AppState>, index: usize, name: String) {
    st.session.lock().unwrap().rename_track(index, name);
}

#[tauri::command]
fn delete_clip(app: AppHandle, st: State<AppState>, track_index: usize, clip_index: usize) {
    st.session.lock().unwrap().delete_clip(track_index, clip_index);
    push_state(&app);
}

fn clip_path(st: &State<AppState>, track_index: usize, clip_index: usize) -> Option<String> {
    let s = st.session.lock().unwrap();
    s.tracks.get(track_index)?.clips.get(clip_index).map(|c| c.path.clone())
}

/// Open the clip's source file in its default app (reveal=false), or show it
/// selected in File Explorer (reveal=true).
#[tauri::command]
fn open_clip(app: AppHandle, st: State<AppState>, track_index: usize, clip_index: usize, reveal: bool) {
    let Some(path) = clip_path(&st, track_index, clip_index) else { return };
    if !std::path::Path::new(&path).exists() {
        set_status(&app, "That file has moved or been deleted", "error");
        return;
    }
    let result = {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut c = std::process::Command::new("explorer.exe");
            if reveal {
                c.raw_arg(format!("/select,\"{path}\""));
            } else {
                c.raw_arg(format!("\"{path}\""));
            }
            c.spawn()
        }
        #[cfg(target_os = "macos")]
        {
            let mut c = std::process::Command::new("open");
            if reveal {
                c.arg("-R");
            }
            c.arg(&path).spawn()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let target = if reveal {
                std::path::Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or(path.clone())
            } else {
                path.clone()
            };
            std::process::Command::new("xdg-open").arg(target).spawn()
        }
    };
    if let Err(e) = result {
        set_status(&app, &format!("Couldn't open the file: {e}"), "error");
    }
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Open the bundled licence notices in the default text viewer.
#[tauri::command]
fn open_notices(app: AppHandle) {
    let path = app
        .path()
        .resource_dir()
        .map(|d| d.join("THIRD-PARTY-NOTICES.txt"))
        .ok()
        .filter(|p| p.exists());
    let Some(path) = path else {
        set_status(&app, "Licence notices not found", "error");
        return;
    };
    #[cfg(windows)]
    let r = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe").raw_arg(format!("\"{}\"", path.display())).spawn()
    };
    #[cfg(not(windows))]
    let r = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(&path).spawn();
    if let Err(e) = r {
        set_status(&app, &format!("Couldn't open licences: {e}"), "error");
    }
}

#[tauri::command]
fn remove_unsynced(app: AppHandle, st: State<AppState>) {
    let n = st.session.lock().unwrap().remove_unsynced();
    push_state_with_status(&app, &format!("Removed {n} unsynced clip{}", if n == 1 { "" } else { "s" }), "default");
}

#[tauri::command]
fn clear_all(app: AppHandle, st: State<AppState>) {
    st.cancel.lock().unwrap().store(true, Ordering::Relaxed);
    *st.generation.lock().unwrap() += 1;
    st.session.lock().unwrap().clear();
    push_state(&app);
}

#[tauri::command]
async fn export_timeline(app: AppHandle, fmt: String, strip_camera_audio: bool, drop_unsynced: bool) -> Value {
    let format = match fmt.as_str() {
        "premiere" => export::Format::Premiere,
        "fcpxml" => export::Format::Fcpxml,
        _ => export::Format::Resolve,
    };
    let (ext, label) = if format == export::Format::Fcpxml { ("fcpxml", "FCPXML") } else { ("xml", "XML") };
    let app2 = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app2.dialog()
            .file()
            .set_title("Export timeline")
            .set_file_name(format!("synced_timeline.{ext}"))
            .add_filter(label, &[ext])
            .blocking_save_file()
    })
    .await
    .ok()
    .flatten();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return json!({"success": false});
    };
    let tracks = app.state::<AppState>().session.lock().unwrap().tracks.clone();
    let opts = export::ExportOptions { strip_camera_audio, drop_unsynced };
    match export::export(&tracks, format, &path.to_string_lossy(), opts) {
        Ok(()) => json!({"success": true, "path": path.file_name().unwrap_or_default().to_string_lossy()}),
        Err(e) => json!({"success": false, "error": e}),
    }
}

/// Debug builds only: SYNCUP_TEST_FILES="a.mxf;b.wav" loads files and syncs on
/// startup, for automated end-to-end checks.
#[cfg(debug_assertions)]
fn test_hook(app: AppHandle) {
    let Ok(list) = std::env::var("SYNCUP_TEST_FILES") else { return };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        enqueue(&app, list.split(';').map(String::from).collect());
        loop {
            std::thread::sleep(std::time::Duration::from_millis(300));
            if app.state::<AppState>().session.lock().unwrap().can_sync() {
                run_sync(app.clone(), app.state::<AppState>());
                return;
            }
        }
    });
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let (tx, rx) = channel::<Job>();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            session: Mutex::new(Session::new()),
            generation: Mutex::new(0),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            jobs: Mutex::new(tx),
        })
        .setup(move |app| {
            start_loaders(app.handle().clone(), rx);
            #[cfg(debug_assertions)]
            test_hook(app.handle().clone());
            if let Err(e) = media::check_tools() {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    set_status(&h, &e, "error");
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            add_files,
            add_files_from_paths,
            run_sync,
            cancel_sync,
            remove_track,
            rename_track,
            delete_clip,
            clear_all,
            remove_unsynced,
            open_clip,
            app_version,
            open_notices,
            export_timeline
        ])
        .run(tauri::generate_context!())
        .expect("error while running ShotSynq");
}
