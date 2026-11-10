//! The Android app: one window, no hotkeys, no pasting into other apps.
//!
//! The page records the microphone itself (getUserMedia) and hands the
//! samples to `transcribe`, which runs the same engines and cleanup as the
//! desktop pipeline. The result goes back to the page, to copy or keep in a
//! note, and into the history.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindowBuilder};

use crate::audio::{self, WHISPER_RATE};
use crate::history::{self, History, HistoryEntry};
use crate::notes::Notes;
use crate::settings::{Engine, Settings};
use crate::{polish, stt, AppState, CmdResult};

/// Anything shorter is an accidental tap.
const MIN_SPEECH_SECS: f32 = 0.3;
/// Peak amplitude below which a recording is treated as silence (about -40 dBFS).
const SILENCE_PEAK: f32 = 0.01;

/// Set while a recording is being transcribed, so a second one waits its turn
/// instead of fighting over the model.
#[derive(Default)]
struct Busy(AtomicBool);

/// What `transcribe` hands back to the page.
#[derive(Serialize)]
pub(crate) struct Dictated {
    text: String,
    raw: String,
    mode: String,
    /// The text model failed; `text` is the cleaned-up transcript.
    warning: Option<String>,
    latency_ms: u64,
    /// The note the text was added to, when one was open for dictation.
    note: Option<crate::notes::Note>,
}

/// There's no llama-server on Android: the text model is always "my server".
fn for_android(mut settings: Settings) -> Settings {
    settings.text_model.engine = Engine::Remote;
    settings.gpu = false;
    settings
}

#[tauri::command]
pub(crate) fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> CmdResult<Vec<String>> {
    let settings = for_android(settings);
    let previous = state.settings.lock().unwrap().clone();
    settings.save(&state.settings_path).map_err(crate::err)?;
    *state.settings.lock().unwrap() = settings.clone();
    if previous.engine != settings.engine || previous.local_model != settings.local_model {
        crate::warm_up(&app);
    }
    if previous.keep_history && !settings.keep_history {
        state.history.lock().unwrap().forget_file();
    }
    Ok(Vec::new())
}

/// The body is the recording as 16-bit little-endian mono PCM; the
/// `x-sample-rate` header gives its rate and `x-summary: 1` asks for summary
/// bullets instead of the text.
#[tauri::command]
pub(crate) async fn transcribe(app: AppHandle, request: tauri::ipc::Request<'_>) -> CmdResult<Dictated> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else {
        return Err("expected the recording as raw bytes".into());
    };
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let rate: u32 = header("x-sample-rate").and_then(|r| r.parse().ok()).unwrap_or(WHISPER_RATE);
    let summary = header("x-summary").as_deref() == Some("1");
    let pcm: Vec<f32> = body
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / i16::MAX as f32)
        .collect();

    let busy = app.state::<Busy>();
    if busy.0.swap(true, Ordering::SeqCst) {
        return Err("Still working on the previous recording".into());
    }
    let result = dictate(&app, pcm, rate, summary).await;
    busy.0.store(false, Ordering::SeqCst);
    result.map_err(|e| format!("{e:#}"))
}

async fn dictate(app: &AppHandle, pcm: Vec<f32>, rate: u32, summary: bool) -> anyhow::Result<Dictated> {
    let samples = tauri::async_runtime::spawn_blocking(move || audio::resample(&pcm, rate, WHISPER_RATE)).await?;
    let secs = samples.len() as f32 / WHISPER_RATE as f32;
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    log::info!("recorded {secs:.2}s at {rate} Hz, peak {peak:.4}");
    if secs < MIN_SPEECH_SECS {
        anyhow::bail!("That was too short to hear anything");
    }
    if peak < SILENCE_PEAK {
        // Whisper invents short phrases for silence; don't keep those.
        anyhow::bail!("Didn't hear anything; check that the app may use the microphone");
    }

    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let started = Instant::now();
    let req = stt::Request::new(samples, &settings);
    let transcript = stt::transcribe(&settings, state.local.clone(), &state.models_dir, req).await?;
    if transcript.text.is_empty() {
        anyhow::bail!("Didn't catch any words");
    }
    let language = transcript.language.as_deref();
    let raw = transcript.text.clone();
    let on_model = |task: &polish::Task| {
        let _ = app.emit("dictation-status", if *task == polish::Task::Summarize { "Summarizing" } else { "Formatting" });
    };
    let shaped = polish::shape(
        &settings,
        &state.text_model,
        &state.models_dir,
        &raw,
        language,
        summary,
        settings.polish,
        settings.format_style,
        on_model,
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as u64;

    let mut mode = shaped.mode;
    let capture = *state.capture.lock().unwrap();
    let note = capture.map(|id| state.notes.lock().unwrap().append(id, &shaped.text));
    if note.is_some() {
        mode = format!("{mode} → note");
    }
    let entry = HistoryEntry {
        text: shaped.text.clone(),
        raw: raw.clone(),
        mode: mode.clone(),
        app: "Android".into(),
        engine: match settings.engine {
            Engine::Local => format!("local · {}", settings.local_model),
            Engine::Remote => format!("server · {}", settings.remote.model),
        },
        latency_ms,
        at: history::now(),
    };
    state.history.lock().unwrap().push(entry.clone(), settings.keep_history);
    let _ = app.emit("history-added", entry);
    Ok(Dictated { text: shaped.text, raw, mode, warning: shaped.warning, latency_ms, note })
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().level(log::LevelFilter::Info).build())
        .manage(Busy::default())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let settings_path = config_dir.join("settings.json");
            let settings = for_android(Settings::load(&settings_path));
            let history_days = if settings.keep_history { settings.history_days } else { 0 };
            let history = History::load(config_dir.join("history.json"), history_days);
            if !settings.keep_history {
                history.forget_file();
            }
            let local = Arc::new(stt::LocalWhisper::default());
            local.set_gpu(false);
            app.manage(AppState {
                settings: Mutex::new(settings),
                settings_path,
                models_dir: data_dir.join("models"),
                local,
                // Never started: `for_android` points the text model at a server.
                text_model: polish::LocalTextModel::new(PathBuf::new()),
                history: Mutex::new(history),
                notes: Mutex::new(Notes::load(config_dir.join("notes.json"))),
                capture: Mutex::new(None),
            });
            // Built here, not from the config, so the page's first invoke
            // can't run before AppState is managed.
            if let Some(main) = app.config().app.windows.iter().find(|w| w.label == "main").cloned() {
                WebviewWindowBuilder::from_config(app, &main)?.build()?;
            }
            crate::warm_up(app.handle());
            Ok(())
        })
        .invoke_handler(crate::mobile_commands())
        .run(tauri::generate_context!())
        .expect("error while running TalyxelVoice");
}
