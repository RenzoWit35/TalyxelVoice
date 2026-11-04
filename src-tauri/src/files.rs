//! Turns folders of recordings (WhatsApp voice notes and other Ogg Opus
//! files) into one transcript per recording: .md, .txt or .srt. Runs on the
//! local whisper with its own model instance, so dictation keeps working
//! while a batch is busy.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use anyhow::{bail, Context};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::audio::WHISPER_RATE;
use crate::polish;
use crate::settings::{self, FileFormat, Polish, Settings};
use crate::stt::{self, LocalWhisper, Segment};

/// Extensions picked up in the source folders.
const EXTENSIONS: &[&str] = &["opus", "ogg", "oga"];
/// The log keeps this many lines for the window.
const LOG_LINES: usize = 300;

// --- decoding ---------------------------------------------------------------

/// Decodes an Ogg Opus file straight to 16 kHz mono, the rate whisper wants;
/// libopus resamples and downmixes itself, so there's no resampling here.
pub fn decode_opus(path: &Path) -> anyhow::Result<Vec<f32>> {
    let file = File::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
    let mut reader = ogg::PacketReader::new(BufReader::new(file));
    let mut decoder: Option<opus::Decoder> = None;
    let mut pre_skip = 0usize;
    let mut saw_tags = false;
    let mut samples = Vec::new();
    // 120 ms, the longest Opus packet, at 16 kHz.
    let mut buf = vec![0f32; WHISPER_RATE as usize * 120 / 1000];
    while let Some(packet) = reader.read_packet().context("not a valid Ogg file")? {
        let data = &packet.data;
        if decoder.is_none() {
            if data.len() < 19 || &data[..8] != b"OpusHead" {
                bail!("not an Opus recording (Ogg Vorbis and other codecs aren't supported)");
            }
            let channels = data[9];
            let mapping = data[18];
            if mapping != 0 && channels > 2 {
                bail!("surround Opus ({channels} channels) isn't supported");
            }
            // Pre-skip is counted at 48 kHz.
            pre_skip = u16::from_le_bytes([data[10], data[11]]) as usize * WHISPER_RATE as usize / 48_000;
            decoder = Some(opus::Decoder::new(WHISPER_RATE, opus::Channels::Mono).context("libopus failed to start")?);
            continue;
        }
        if !saw_tags && data.starts_with(b"OpusTags") {
            saw_tags = true;
            continue;
        }
        let n = decoder.as_mut().unwrap().decode_float(data, &mut buf, false);
        match n {
            Ok(n) => samples.extend_from_slice(&buf[..n]),
            // One damaged packet shouldn't sink an hour-long recording.
            Err(e) => log::warn!("{}: skipped a bad Opus packet: {e}", path.display()),
        }
    }
    if decoder.is_none() {
        bail!("empty file");
    }
    samples.drain(..pre_skip.min(samples.len()));
    Ok(samples)
}

// --- output -----------------------------------------------------------------

fn clock(ms: u64, sep: char) -> String {
    let (h, m, s, milli) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    format!("{h:02}:{m:02}:{s:02}{sep}{milli:03}")
}

fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Segments joined into paragraphs, with a break wherever the speaker
/// paused for a couple of seconds.
fn paragraphs(segments: &[Segment]) -> String {
    let mut out = String::new();
    let mut prev_end = None;
    for s in segments {
        match prev_end {
            None => {}
            Some(end) if s.start_ms >= end + 2000 => out.push_str("\n\n"),
            Some(_) => out.push(' '),
        }
        out.push_str(&s.text);
        prev_end = Some(s.end_ms);
    }
    out
}

pub struct Recording<'a> {
    pub name: &'a str,
    pub modified: Option<SystemTime>,
    pub seconds: u64,
    pub language: Option<&'a str>,
    pub segments: &'a [Segment],
}

pub fn render(format: FileFormat, r: &Recording) -> String {
    match format {
        FileFormat::Txt => {
            let mut text = paragraphs(r.segments);
            text.push('\n');
            text
        }
        FileFormat::Md => {
            let mut meta = Vec::new();
            if let Some(t) = r.modified {
                meta.push(chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string());
            }
            meta.push(duration(r.seconds));
            if let Some(lang) = r.language {
                meta.push(lang.to_owned());
            }
            let body = paragraphs(r.segments);
            let body = if body.is_empty() { "_(no speech recognized)_".to_owned() } else { body };
            format!("# {}\n\n*{}*\n\n{}\n", r.name, meta.join(" · "), body)
        }
        FileFormat::Srt => {
            let mut out = String::new();
            for (i, s) in r.segments.iter().enumerate() {
                let end = s.end_ms.max(s.start_ms + 500);
                out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, clock(s.start_ms, ','), clock(end, ','), s.text));
            }
            out
        }
    }
}

// --- finding the work -------------------------------------------------------

pub struct Job {
    pub source: PathBuf,
    pub output: PathBuf,
    pub bytes: u64,
}

fn is_recording(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if recursive {
                walk(&path, recursive, out)?;
            }
        } else if kind.is_file() && is_recording(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Every recording in `folders`, each with where its transcript goes:
/// `target/<folder name>/<subfolders>/<recording>.<ext>`. Two source folders
/// with the same name get "name (2)", so nothing is overwritten.
pub fn plan(folders: &[String], target: &Path, recursive: bool, format: FileFormat) -> anyhow::Result<Vec<Job>> {
    let mut jobs = Vec::new();
    let mut used: Vec<String> = Vec::new();
    for folder in folders {
        let dir = PathBuf::from(folder.trim());
        if !dir.is_dir() {
            bail!("folder not found: {}", dir.display());
        }
        let base = dir.file_name().map_or("recordings".into(), |n| n.to_string_lossy().into_owned());
        let mut name = base.clone();
        let mut n = 2;
        while used.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
            name = format!("{base} ({n})");
            n += 1;
        }
        used.push(name.clone());

        let mut found = Vec::new();
        walk(&dir, recursive, &mut found).with_context(|| format!("couldn't read {}", dir.display()))?;
        found.sort();
        for source in found {
            let rel = source.strip_prefix(&dir).unwrap_or(&source);
            let output = target.join(&name).join(rel).with_extension(format.extension());
            let bytes = source.metadata().map_or(0, |m| m.len());
            jobs.push(Job { source, output, bytes });
        }
    }
    Ok(jobs)
}

// --- the batch --------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub ok: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub running: bool,
    pub stopping: bool,
    pub total: usize,
    pub done: usize,
    pub ok: usize,
    pub failed: usize,
    pub skipped: usize,
    /// The recording being transcribed and whisper's progress on it.
    pub current: String,
    pub percent: i32,
    pub eta_secs: Option<u64>,
    pub target: String,
    pub log: Vec<LogLine>,
}

#[derive(Default)]
pub struct Batch {
    status: Mutex<Status>,
    stop: AtomicBool,
}

impl Batch {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn request_stop(&self, app: &AppHandle) {
        self.stop.store(true, Ordering::Relaxed);
        self.update(app, |s| s.stopping = s.running);
    }

    fn update(&self, app: &AppHandle, f: impl FnOnce(&mut Status)) {
        let snapshot = {
            let mut s = self.status.lock().unwrap();
            f(&mut s);
            if s.log.len() > LOG_LINES {
                let extra = s.log.len() - LOG_LINES;
                s.log.drain(..extra);
            }
            s.clone()
        };
        let _ = app.emit("files-progress", snapshot);
    }

    fn log(&self, app: &AppHandle, ok: bool, text: String) {
        self.update(app, |s| s.log.push(LogLine { ok, text }));
    }
}

/// Plans the work and starts it on its own thread.
pub fn start(app: AppHandle, batch: Arc<Batch>, settings: Settings, models_dir: PathBuf) -> anyhow::Result<()> {
    let opts = &settings.files;
    if batch.status().running {
        bail!("a batch is already running");
    }
    let folders: Vec<String> = opts.folders.iter().filter(|f| !f.trim().is_empty()).cloned().collect();
    if folders.is_empty() {
        bail!("add at least one folder with recordings");
    }
    if opts.target.trim().is_empty() {
        bail!("pick a folder to save the transcripts in");
    }
    let model = settings::model_path(&models_dir, &settings.local_model);
    if !model.exists() {
        bail!("the local model {} isn't downloaded yet; get it under Engine", settings.local_model);
    }
    let target = PathBuf::from(opts.target.trim());
    let jobs = plan(&folders, &target, opts.recursive, opts.format)?;
    if jobs.is_empty() {
        bail!("no .opus or .ogg recordings in those folders");
    }

    batch.stop.store(false, Ordering::Relaxed);
    batch.update(&app, |s| {
        *s = Status { running: true, total: jobs.len(), target: target.display().to_string(), ..Default::default() };
        s.log.push(LogLine {
            ok: true,
            text: format!("{} recordings, model {}", jobs.len(), settings.local_model),
        });
    });
    std::thread::spawn(move || {
        run(&app, &batch, &settings, &model, jobs);
        let stopped = batch.stop.load(Ordering::Relaxed);
        batch.update(&app, |s| {
            s.running = false;
            s.stopping = false;
            s.current.clear();
            s.percent = 0;
            s.eta_secs = None;
            let word = if stopped { "Stopped" } else { "Done" };
            s.log.push(LogLine {
                ok: s.failed == 0,
                text: format!("{word}: {} transcribed, {} failed, {} skipped", s.ok, s.failed, s.skipped),
            });
        });
    });
    Ok(())
}

fn run(app: &AppHandle, batch: &Arc<Batch>, settings: &Settings, model: &Path, jobs: Vec<Job>) {
    // A model of its own: a long recording would otherwise hold up dictation.
    let whisper = LocalWhisper::default();
    whisper.set_gpu(settings.gpu);
    let total_bytes: u64 = jobs.iter().map(|j| j.bytes).sum::<u64>().max(1);
    let mut skipped_bytes = 0u64;
    let mut done_bytes = 0u64;
    let started = Instant::now();
    let replacements = settings.replacements();

    for job in jobs {
        if batch.stop.load(Ordering::Relaxed) {
            break;
        }
        let name = job.source.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        if settings.files.skip_existing && job.output.exists() {
            skipped_bytes += job.bytes;
            batch.update(app, |s| {
                s.done += 1;
                s.skipped += 1;
            });
            continue;
        }
        batch.update(app, |s| {
            s.current = name.clone();
            s.percent = 0;
        });

        // Bytes/second over what was actually transcribed; Opus is close
        // enough to constant bitrate for that to track audio length.
        let eta = {
            let batch = batch.clone();
            let app = app.clone();
            let (bytes, size) = (done_bytes, job.bytes);
            let remaining = total_bytes - skipped_bytes - done_bytes;
            let last = Arc::new(Mutex::new(-1));
            move |pct: i32| {
                let mut last = last.lock().unwrap();
                if pct == *last {
                    return;
                }
                *last = pct;
                let elapsed = started.elapsed().as_secs_f64();
                let now = bytes as f64 + size as f64 * pct as f64 / 100.0;
                let eta = (now > 0.0 && elapsed > 3.0)
                    .then(|| (elapsed / now * (remaining as f64 - size as f64 * pct as f64 / 100.0).max(0.0)) as u64);
                batch.update(&app, |s| {
                    s.percent = pct;
                    s.eta_secs = eta;
                });
            }
        };
        let abort = {
            let batch = batch.clone();
            move || batch.stop.load(Ordering::Relaxed)
        };

        let result = transcribe_one(&whisper, settings, model, &job, &replacements, eta, abort);
        done_bytes += job.bytes;
        if batch.stop.load(Ordering::Relaxed) {
            // Aborted mid-file: no transcript, and not a failure.
            break;
        }
        let rel = job.output.display().to_string();
        match result {
            Ok(secs) => {
                batch.update(app, |s| {
                    s.done += 1;
                    s.ok += 1;
                });
                batch.log(app, true, format!("{name} ({}) → {rel}", duration(secs)));
            }
            Err(e) => {
                batch.update(app, |s| {
                    s.done += 1;
                    s.failed += 1;
                });
                batch.log(app, false, format!("{name}: {e:#}"));
            }
        }
    }
}

/// Decodes, transcribes and writes one recording; returns its length in seconds.
fn transcribe_one(
    whisper: &LocalWhisper,
    settings: &Settings,
    model: &Path,
    job: &Job,
    replacements: &[(String, String)],
    progress: impl FnMut(i32) + 'static,
    abort: impl FnMut() -> bool + 'static,
) -> anyhow::Result<u64> {
    let samples = decode_opus(&job.source)?;
    let seconds = samples.len() as u64 / WHISPER_RATE as u64;
    let req = stt::Request::new(samples, settings);
    let (mut segments, language) = whisper.transcribe_file(model, &req, progress, abort)?;
    for s in &mut segments {
        if settings.polish != Polish::Off {
            s.text = polish::strip_fillers(&s.text, language.as_deref());
        }
        s.text = polish::replace_words(&s.text, replacements);
    }
    segments.retain(|s| !s.text.trim().is_empty());

    let name = job.source.file_stem().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let modified = job.source.metadata().and_then(|m| m.modified()).ok();
    let text = render(
        settings.files.format,
        &Recording { name: &name, modified, seconds, language: language.as_deref(), segments: &segments },
    );
    if let Some(dir) = job.output.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    }
    // Written under a temporary name first, so a crash never leaves a
    // half-written file that the next run would skip as done.
    let part = job.output.with_extension("part");
    std::fs::write(&part, text).with_context(|| format!("couldn't write {}", part.display()))?;
    std::fs::rename(&part, &job.output).with_context(|| format!("couldn't write {}", job.output.display()))?;
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment { start_ms, end_ms, text: text.into() }
    }

    #[test]
    fn srt_timestamps() {
        let segments = [seg(0, 1500, "Hallo."), seg(3_725_040, 3_726_000, "Doei.")];
        let r = Recording { name: "x", modified: None, seconds: 3726, language: Some("nl"), segments: &segments };
        assert_eq!(
            render(FileFormat::Srt, &r),
            "1\n00:00:00,000 --> 00:00:01,500\nHallo.\n\n2\n01:02:05,040 --> 01:02:06,000\nDoei.\n\n"
        );
    }

    #[test]
    fn markdown_paragraphs_on_pauses() {
        let segments = [seg(0, 1000, "Een."), seg(1000, 2000, "Twee."), seg(5000, 6000, "Drie.")];
        let r = Recording { name: "PTT-1", modified: None, seconds: 65, language: Some("nl"), segments: &segments };
        assert_eq!(render(FileFormat::Md, &r), "# PTT-1\n\n*1:05 · nl*\n\nEen. Twee.\n\nDrie.\n");
        assert_eq!(render(FileFormat::Txt, &r), "Een. Twee.\n\nDrie.\n");
        let empty = Recording { segments: &[], ..r };
        assert!(render(FileFormat::Md, &empty).ends_with("_(no speech recognized)_\n"));
    }

    #[test]
    fn plan_mirrors_folders_and_dedupes_names() {
        let root = std::env::temp_dir().join(format!("talyxel-files-{}", std::process::id()));
        let a = root.join("one").join("Voice");
        let b = root.join("two").join("Voice");
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        for p in [a.join("x.opus"), a.join("sub").join("y.OGG"), a.join("notes.txt"), b.join("z.opus")] {
            std::fs::write(p, b"").unwrap();
        }
        let target = root.join("out");
        let folders = [a.display().to_string(), b.display().to_string()];
        let flat = plan(&folders, &target, false, FileFormat::Md).unwrap();
        let outs: Vec<_> = flat.iter().map(|j| j.output.strip_prefix(&target).unwrap().to_path_buf()).collect();
        assert_eq!(outs, [PathBuf::from("Voice/x.md"), PathBuf::from("Voice (2)/z.md")]);
        let deep = plan(&folders, &target, true, FileFormat::Srt).unwrap();
        assert!(deep.iter().any(|j| j.output.ends_with("Voice/sub/y.srt")));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `TALYXEL_TEST_OPUS=voice.opus cargo test decodes_opus -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decodes_opus() {
        let path = std::env::var("TALYXEL_TEST_OPUS").expect("TALYXEL_TEST_OPUS");
        let samples = decode_opus(Path::new(&path)).unwrap();
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!("{} samples ({:.1} s), peak {peak:.2}", samples.len(), samples.len() as f32 / 16000.0);
        assert!(samples.len() > 16000 && peak > 0.01);
    }
}
