<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="128" alt="ShotSynq icon">

# ShotSynq

### Free, open-source multicam and audio sync

**Drop in the whole shoot. Get a synced timeline.**

<a href="../../releases/latest"><img src="https://img.shields.io/badge/Download_for_Windows-Windows_10_%2F_11_·_x64-1f6feb?style=for-the-badge&logo=windows&logoColor=white" alt="Download for Windows" height="44"></a>

Version 0.10.0 · [All releases](../../releases)

A free, open source alternative to PluralEyes and Syncaila. Add multiple camera and audio sources from a shoot, and align using audio waveforms. **Export to Adobe Premiere Pro, DaVinci Resolve or Final Cut Pro**. No timecode needed.

https://github.com/user-attachments/assets/16e9d0ec-bf21-4b6c-a69e-11fb5f86995c

<img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT licence"> <img src="https://img.shields.io/badge/platform-Windows-0078d4" alt="Windows">

</div>
---

## Doesn't my editor already do this?

For one camera and one recorder, often yes. For a whole shoot, not well:

- **Track sprawl.** Built-in sync often puts each recording on a new track. ShotSynq keeps one track per camera and recorder, however many times they started and stopped.
- **One shot at a time.** Some editors only sync a handful of clips, or a single shot, per go. ShotSynq takes the entire shoot and gives you one timeline.
- **Silent failures.** Clips it can't prove go in red at the end, and weak matches are flagged for you to check, rather than being placed wrongly without a word.

## How to use it

1. **Add media.** Drag in files or whole card folders. Each camera and recorder gets its own track.
2. **Synchronise.** Clips slide into place, usually in about a second.
3. **Check.** Weak matches are marked in amber: **Keep** or **Remove** each one. Anything that couldn't be synced sits in red at the end of the timeline. Move clips by dragging or with `,` and `.`; undo with Ctrl+Z.
4. **Export.** Pick your editor. Your original files are referenced, never copied or changed.

**Works with** anything FFmpeg can read with an audio track: MP4, MOV, MXF, MTS, MKV, WAV/BWF, FLAC, MP3 and more, including **Blackmagic RAW** and **ARRI MXF**.

## How does it work?

**1. Reading the files.** Cameras use dozens of containers and codecs. Rather than writing readers for each, ShotSynq calls [FFmpeg](https://ffmpeg.org), the open-source tool behind most video software. FFprobe reads each file's details (length, frame rate, timecode, camera model) and FFmpeg decodes just the sound to 16 kHz mono. Pictures are never decoded, which is why raw formats work: only their audio is needed. FFmpeg runs as a separate program and ships with the app.

**2. Turning sound into a fingerprint.** Each clip's audio is split into 6 frequency bands, 100 times a second. Levels are evened out, so what's left is the *rhythm* of the sound, not its loudness or tone. That's why a camera's on-board mic still matches a lav: different sound quality, same rhythm.

**3. Comparing every pair.** Every clip is compared with every clip from the other devices, at every possible offset at once, using FFT cross-correlation. A match only counts if it stands out clearly from all the other offsets. Repetitive sound that matches in two places is rejected.

**4. Building the timeline.** Matches are accepted strongest first, and refused if they'd put two clips from one camera on top of each other, since a camera can't record twice at once. All matches are then solved together, so small errors average out. A clip that matches in several places at once, like an edit exported by mistake, is spotted and left out.

**5. Exporting.** The result is written as FCP7 XML (Premiere and Resolve) or FCPXML (Final Cut Pro), pointing at your original files.

## Limitations

- Clips must have recorded the same sound. Timecode-only sync isn't supported yet.
- RED (.r3d) isn't readable by FFmpeg: sync the proxies, then relink in your editor.
- No clock-drift correction yet, so very long takes may slip by a frame or so.
- Windows only. Final Cut Pro export is untested. The installer isn't code-signed, so SmartScreen will warn once: **More info → Run anyway**.

## Build from source

Needs Windows 10/11, [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (Desktop development with C++), [Rust](https://rustup.rs) and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

```powershell
powershell -ExecutionPolicy Bypass -File .\setup-ffmpeg.ps1   # fetches FFmpeg
cargo tauri dev      # run
cargo tauri build    # build the installer
```

| Path | What's in it |
|---|---|
| `crates/shotsynq-core/src/engine.rs` | The sync engine (settings at the top) |
| `crates/shotsynq-core/src/media.rs` | Reading media through FFmpeg |
| `crates/shotsynq-core/src/export.rs` | XML export |
| `crates/shotsynq-core/src/session.rs` | Tracks, edits and undo |
| `src-tauri/`, `ui/` | The app window and interface |

## Licence

[MIT](LICENSE). The installer includes unmodified FFmpeg executables (GPLv3), run as separate programs; see [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt) or **Licences** in the app.

---

<div align="center">
Made by Rory Marshall
</div>
