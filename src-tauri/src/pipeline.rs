//! Hotkey → record → transcribe → paste.

use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::audio::{self, Recording, WHISPER_RATE};
use crate::history::{self, HistoryEntry};
use crate::platform::{self, FocusedApp};
use crate::polish::{self, Task};
use crate::settings::{Engine, Polish, Settings};
use crate::stt;
use crate::AppState;

/// Releases quicker than this count as a tap, which toggles hands-free mode.
const TAP: Duration = Duration::from_millis(350);
/// Anything shorter is an accidental press.
const MIN_SPEECH_SECS: f32 = 0.3;
/// Peak amplitude below which a recording is treated as silence (about -40 dBFS).
const SILENCE_PEAK: f32 = 0.01;

/// Which hotkey started a recording, which decides what the words become.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Pasted as text, after the configured cleanup.
    Dictate,
    /// Pasted as a few summary bullets.
    Summary,
    /// Not pasted: it's an instruction, applied to the selected text.
    Command,
}

/// Selections longer than this aren't sent to the model: too slow locally,
/// and past what the prompt examples cover.
const MAX_SELECTION_WORDS: usize = 3000;

pub enum Dictation {
    Idle,
    Recording {
        rec: Recording,
        started: Instant,
        hands_free: bool,
        kind: Kind,
        app: FocusedApp,
    },
    Busy,
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Pill {
    Listening { hands_free: bool, kind: Kind },
    Level { level: f32 },
    Transcribing,
    Polishing { summary: bool },
    Working { message: String },
    Info { message: String },
    Error { message: String },
}

fn pill(app: &AppHandle, p: Pill) {
    let _ = app.emit_to("pill", "pill", p);
}

fn show_pill(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("pill") {
        if let Ok(hwnd) = w.hwnd() {
            platform::show_without_focus(hwnd.0 as _);
        }
    }
}

fn hide_pill(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("pill") {
        if let Ok(hwnd) = w.hwnd() {
            platform::hide_window(hwnd.0 as _);
        }
    }
}

/// While recording, only the hotkey that started it can stop it.
pub fn on_hotkey(app: &AppHandle, pressed: bool, kind: Kind) {
    let state = app.state::<AppState>();
    let mut dictation = state.dictation.lock().unwrap();
    if let Dictation::Recording { kind: started_with, .. } = &*dictation {
        if *started_with != kind {
            return;
        }
    }
    match (&mut *dictation, pressed) {
        (Dictation::Idle, true) => {
            let focused = platform::focused_app();
            let settings = state.settings.lock().unwrap().clone();
            match Recording::start(&settings.microphone) {
                Ok(rec) => {
                    let (polish, _) = settings.mode_for(&focused.exe);
                    *dictation = Dictation::Recording {
                        rec,
                        started: Instant::now(),
                        hands_free: false,
                        kind,
                        app: focused,
                    };
                    // Load the text model while the user talks.
                    let needs_model = kind != Kind::Dictate || matches!(polish, Polish::Medium | Polish::Format);
                    if needs_model {
                        prestart_text_model(&state, &settings);
                    }
                    drop(dictation);
                    pill(app, Pill::Listening { hands_free: false, kind });
                    show_pill(app);
                    spawn_level_meter(app.clone());
                }
                Err(e) => {
                    drop(dictation);
                    report_error(app.clone(), format!("Microphone: {e}"));
                }
            }
        }
        (Dictation::Recording { started, hands_free, .. }, false) if !*hands_free => {
            if started.elapsed() < TAP {
                *hands_free = true;
                drop(dictation);
                pill(app, Pill::Listening { hands_free: true, kind });
            } else {
                drop(dictation);
                finish(app.clone());
            }
        }
        (Dictation::Recording { hands_free: true, .. }, true) => {
            drop(dictation);
            finish(app.clone());
        }
        // Key repeat, the release after a hands-free stop, or presses while busy.
        _ => {}
    }
}

fn prestart_text_model(state: &AppState, settings: &Settings) {
    if settings.text_model.engine == Engine::Local {
        let model = crate::settings::text_model_path(&state.models_dir, &settings.text_model.local_model);
        state.text_model.prestart(model);
    }
}

fn spawn_level_meter(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let level = match &*app.state::<AppState>().dictation.lock().unwrap() {
                Dictation::Recording { rec, .. } => rec.level(),
                _ => break,
            };
            pill(&app, Pill::Level { level });
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
}

fn finish(app: AppHandle) {
    let state = app.state::<AppState>();
    let (rec, focused, kind) = {
        let mut dictation = state.dictation.lock().unwrap();
        match std::mem::replace(&mut *dictation, Dictation::Busy) {
            Dictation::Recording { rec, app, kind, .. } => (rec, app, kind),
            other => {
                *dictation = other;
                return;
            }
        }
    };
    pill(&app, Pill::Transcribing);

    tauri::async_runtime::spawn(async move {
        let result = run(&app, rec, &focused, kind).await;
        let state = app.state::<AppState>();
        *state.dictation.lock().unwrap() = Dictation::Idle;
        match result {
            Ok(Outcome::Done) => hide_pill(&app),
            Ok(Outcome::Notice(message)) => notice(app.clone(), &message),
            Ok(Outcome::Warning(warning)) | Err(warning) => report_error(app.clone(), warning.to_string()),
        }
    });
}

enum Outcome {
    Done,
    /// Went fine, but say so (the text didn't visibly appear anywhere).
    Notice(String),
    /// The text went in, but not as asked (the model failed; plain text pasted).
    Warning(anyhow::Error),
}

async fn run(
    app: &AppHandle,
    rec: Recording,
    focused: &FocusedApp,
    kind: Kind,
) -> anyhow::Result<Outcome> {
    let samples = tauri::async_runtime::spawn_blocking(move || rec.stop()).await?;
    let secs = samples.len() as f32 / WHISPER_RATE as f32;
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    log::info!("recorded {secs:.2}s, peak {peak:.4}, into {:?}", focused.exe);

    let state = app.state::<AppState>();
    #[cfg(debug_assertions)]
    if let Some(dir) = state.models_dir.parent() {
        // Dev builds only: lets you listen to exactly what whisper got.
        let _ = audio::to_wav(&samples).map(|wav| std::fs::write(dir.join("last-dictation.wav"), wav));
    }
    if secs < MIN_SPEECH_SECS {
        return Ok(Outcome::Done);
    }
    if peak < SILENCE_PEAK {
        // Whisper invents short phrases for silence; don't paste those.
        anyhow::bail!("Didn't hear anything — check the microphone in Settings");
    }

    let settings = state.settings.lock().unwrap().clone();
    let started = Instant::now();
    let req = stt::Request::new(samples, &settings);
    let transcript = stt::transcribe(&settings, state.local.clone(), &state.models_dir, req).await?;
    if transcript.text.is_empty() {
        return Ok(Outcome::Done);
    }
    let language = transcript.language.as_deref();
    let raw = transcript.text;

    if kind == Kind::Command {
        let instruction = polish::strip_fillers(&raw, language);
        log::info!("command: {instruction:?}");
        rewrite_selection(app, &settings, focused, Task::Command(instruction), language).await?;
        return Ok(Outcome::Done);
    }

    let summary = kind == Kind::Summary;
    let (polish, format_style) = settings.mode_for(&focused.exe);
    // Dictionary fixes and spoken punctuation are the user's own rules, so
    // they apply even to "as spoken".
    let mut text = polish::replace_words(&raw, &settings.replacements());
    if settings.spoken_punctuation {
        text = polish::spoken_punctuation(&text);
    }
    if polish != Polish::Off || summary {
        text = polish::strip_fillers(&text, language);
    }
    let snippet = if summary { None } else { polish::apply_snippet(&text, &settings.snippets) };
    let (task, mut mode) = match snippet {
        Some(expanded) => {
            text = expanded;
            (None, "snippet".to_owned())
        }
        None if summary => (Some(Task::Summarize), "summary".to_owned()),
        None => match polish {
            Polish::Off => (None, "as spoken".to_owned()),
            Polish::Clean => (None, "clean".to_owned()),
            Polish::Medium => (Some(Task::Cleanup), "medium".to_owned()),
            Polish::Format => (
                Some(Task::Format(format_style)),
                format!("format · {}", format!("{format_style:?}").to_lowercase()),
            ),
        },
    };
    let mut warning = None;
    if let Some(task) = task {
        pill(app, Pill::Polishing { summary: task == Task::Summarize });
        let cfg = &settings.text_model;
        match polish::apply(cfg, &state.text_model, &state.models_dir, &task, &text, language).await {
            Ok(polished) => text = polished,
            Err(e) => {
                log::warn!("{task:?} failed: {e:#}");
                let what = match task {
                    Task::Summarize => "summarize",
                    Task::Cleanup => "clean up",
                    _ => "format",
                };
                warning = Some(anyhow::anyhow!("Couldn't {what} ({e}); pasted the plain text"));
                mode = "clean (model failed)".to_owned();
            }
        }
    }
    let latency_ms = started.elapsed().as_millis() as u64;

    // Into the open note instead of the focused app, when the user asked for that.
    let capture = *state.capture.lock().unwrap();
    let outcome = match capture {
        Some(id) => {
            let note = state.notes.lock().unwrap().append(id, &text);
            let _ = app.emit("note-updated", note);
            mode = format!("{mode} → note");
            Outcome::Notice("Added to the note".into())
        }
        None => {
            paste(&settings, text.clone()).await?;
            match warning {
                Some(w) => Outcome::Warning(w),
                None => Outcome::Done,
            }
        }
    };

    record(
        app,
        &settings,
        HistoryEntry {
            text,
            raw,
            mode,
            app: focused.exe.clone(),
            engine: engine_label(&settings),
            latency_ms,
            at: history::now(),
        },
    );
    Ok(outcome)
}

fn engine_label(settings: &Settings) -> String {
    match settings.engine {
        Engine::Local => format!("local · {}", settings.local_model),
        Engine::Remote => format!("server · {}", settings.remote.model),
    }
}

fn text_model_label(settings: &Settings) -> String {
    let cfg = &settings.text_model;
    match cfg.engine {
        Engine::Local => format!("local · {}", cfg.local_model),
        Engine::Remote => format!("server · {}", if cfg.remote.model.is_empty() { "default" } else { &cfg.remote.model }),
    }
}

/// The transform hotkey: rewrites the selected text with the configured preset.
pub fn transform(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut dictation = state.dictation.lock().unwrap();
        if !matches!(*dictation, Dictation::Idle) {
            return;
        }
        *dictation = Dictation::Busy;
    }
    let settings = state.settings.lock().unwrap().clone();
    prestart_text_model(&state, &settings);
    let focused = platform::focused_app();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let task = Task::Transform(settings.transform);
        let result = rewrite_selection(&app, &settings, &focused, task, None).await;
        *app.state::<AppState>().dictation.lock().unwrap() = Dictation::Idle;
        match result {
            Ok(()) => hide_pill(&app),
            Err(e) => report_error(app.clone(), e.to_string()),
        }
    });
}

/// Copies what the focused app has selected, runs `task` on it, and pastes
/// the result over the selection. The original lands in the history, so the
/// undo hotkey brings it back.
async fn rewrite_selection(
    app: &AppHandle,
    settings: &Settings,
    focused: &FocusedApp,
    task: Task,
    language: Option<&str>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let selection = tauri::async_runtime::spawn_blocking(platform::copy_selection).await??;
    let Some(selection) = selection.filter(|s| !s.trim().is_empty()) else {
        anyhow::bail!("Select some text first");
    };
    let words = selection.split_whitespace().count();
    if words > MAX_SELECTION_WORDS {
        anyhow::bail!("That's {words} words; select at most {MAX_SELECTION_WORDS}");
    }
    // The selection's own language counts most: whisper only knows the
    // language of the spoken instruction, and the transform hotkey has no
    // audio at all. A short or mixed selection falls back to those.
    let detected = polish::detect_language(&selection);
    let language = detected.or(language).or_else(|| settings.language_code());
    log::info!("selection language: {language:?} (detected {detected:?})");
    let message = match &task {
        Task::Command(instruction) => format!("“{instruction}”"),
        Task::Transform(t) => format!("{t:?}"),
        _ => "Working".to_owned(),
    };
    pill(app, Pill::Working { message });
    show_pill(app);

    let state = app.state::<AppState>();
    let cfg = &settings.text_model;
    let text = polish::apply(cfg, &state.text_model, &state.models_dir, &task, &selection, language)
        .await
        .map_err(|e| anyhow::anyhow!("Couldn't rewrite it: {e}"))?;
    let latency_ms = started.elapsed().as_millis() as u64;

    paste(settings, text.clone()).await?;
    record(
        app,
        settings,
        HistoryEntry {
            text,
            raw: selection,
            mode: task.label(),
            app: focused.exe.clone(),
            engine: text_model_label(settings),
            latency_ms,
            at: history::now(),
        },
    );
    Ok(())
}

async fn paste(settings: &Settings, text: String) -> anyhow::Result<()> {
    let restore = settings.restore_clipboard;
    tauri::async_runtime::spawn_blocking(move || platform::insert_text(&text, restore)).await?
}

fn record(app: &AppHandle, settings: &Settings, entry: HistoryEntry) {
    let state = app.state::<AppState>();
    state.history.lock().unwrap().push(entry.clone(), settings.keep_history);
    let _ = app.emit("history-added", entry);
}

/// The undo and re-paste hotkeys: pastes the last dictation again, as heard
/// (`original`) or as it went in.
pub fn paste_last(app: &AppHandle, original: bool) {
    let state = app.state::<AppState>();
    if !matches!(*state.dictation.lock().unwrap(), Dictation::Idle) {
        return;
    }
    let last = state.history.lock().unwrap().last().cloned();
    let Some(entry) = last else {
        report_error(app.clone(), "Nothing dictated yet".into());
        return;
    };
    if original && entry.raw == entry.text {
        report_error(app.clone(), "Nothing to undo: it was pasted as heard".into());
        return;
    }
    let settings = state.settings.lock().unwrap().clone();
    let text = if original { entry.raw } else { entry.text };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match paste(&settings, text).await {
            Ok(()) => notice(app, if original { "Pasted the original" } else { "Pasted again" }),
            Err(e) => report_error(app, e.to_string()),
        }
    });
}

fn notice(app: AppHandle, message: &str) {
    pill(&app, Pill::Info { message: message.to_owned() });
    show_pill(&app);
    hide_later(app, 1200);
}

fn report_error(app: AppHandle, message: String) {
    log::error!("{message}");
    pill(&app, Pill::Error { message });
    show_pill(&app);
    hide_later(app, 3000);
}

fn hide_later(app: AppHandle, ms: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        if matches!(*app.state::<AppState>().dictation.lock().unwrap(), Dictation::Idle) {
            hide_pill(&app);
        }
    });
}
