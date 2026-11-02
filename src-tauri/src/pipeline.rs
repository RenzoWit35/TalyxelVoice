//! Hotkey → record → transcribe → paste.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::audio::{self, Recording, WHISPER_RATE};
use crate::platform::{self, FocusedApp};
use crate::stt;
use crate::AppState;

/// Releases quicker than this count as a tap, which toggles hands-free mode.
const TAP: Duration = Duration::from_millis(350);
/// Anything shorter is an accidental press.
const MIN_SPEECH_SECS: f32 = 0.3;
/// Peak amplitude below which a recording is treated as silence (about -40 dBFS).
const SILENCE_PEAK: f32 = 0.01;
const HISTORY_LEN: usize = 50;

pub enum Dictation {
    Idle,
    Recording {
        rec: Recording,
        started: Instant,
        hands_free: bool,
        app: FocusedApp,
    },
    Busy,
}

#[derive(Clone, Serialize)]
pub struct HistoryEntry {
    pub text: String,
    pub app: String,
    pub engine: String,
    pub latency_ms: u128,
    pub at: u64,
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Pill {
    Listening { hands_free: bool },
    Level { level: f32 },
    Transcribing,
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

pub fn on_hotkey(app: &AppHandle, pressed: bool) {
    let state = app.state::<AppState>();
    let mut dictation = state.dictation.lock().unwrap();
    match (&mut *dictation, pressed) {
        (Dictation::Idle, true) => {
            let focused = platform::focused_app();
            let mic = state.settings.lock().unwrap().microphone.clone();
            match Recording::start(&mic) {
                Ok(rec) => {
                    *dictation = Dictation::Recording {
                        rec,
                        started: Instant::now(),
                        hands_free: false,
                        app: focused,
                    };
                    drop(dictation);
                    pill(app, Pill::Listening { hands_free: false });
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
                pill(app, Pill::Listening { hands_free: true });
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
    let (rec, focused) = {
        let mut dictation = state.dictation.lock().unwrap();
        match std::mem::replace(&mut *dictation, Dictation::Busy) {
            Dictation::Recording { rec, app, .. } => (rec, app),
            other => {
                *dictation = other;
                return;
            }
        }
    };
    pill(&app, Pill::Transcribing);

    tauri::async_runtime::spawn(async move {
        let result = run(&app, rec, &focused).await;
        let state = app.state::<AppState>();
        *state.dictation.lock().unwrap() = Dictation::Idle;
        match result {
            Ok(()) => hide_pill(&app),
            Err(e) => report_error(app.clone(), e.to_string()),
        }
    });
}

async fn run(app: &AppHandle, rec: Recording, focused: &FocusedApp) -> anyhow::Result<()> {
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
        return Ok(());
    }
    if peak < SILENCE_PEAK {
        // Whisper invents short phrases for silence; don't paste those.
        anyhow::bail!("Didn't hear anything — check the microphone in Settings");
    }

    let settings = state.settings.lock().unwrap().clone();
    let started = Instant::now();
    let req = stt::Request::new(samples, &settings);
    let text = stt::transcribe(&settings, state.local.clone(), &state.models_dir, req).await?;
    let latency_ms = started.elapsed().as_millis();
    if text.is_empty() {
        return Ok(());
    }

    let restore = settings.restore_clipboard;
    let paste_text = text.clone();
    tauri::async_runtime::spawn_blocking(move || platform::insert_text(&paste_text, restore))
        .await??;

    let entry = HistoryEntry {
        text,
        app: focused.exe.clone(),
        engine: match settings.engine {
            crate::settings::Engine::Local => format!("local · {}", settings.local_model),
            crate::settings::Engine::Remote => format!("server · {}", settings.remote.model),
        },
        latency_ms,
        at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    };
    {
        let mut history = state.history.lock().unwrap();
        history.push_front(entry.clone());
        history.truncate(HISTORY_LEN);
    }
    let _ = app.emit("history-added", entry);
    Ok(())
}

fn report_error(app: AppHandle, message: String) {
    log::error!("{message}");
    pill(&app, Pill::Error { message });
    show_pill(&app);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(3000)).await;
        if matches!(*app.state::<AppState>().dictation.lock().unwrap(), Dictation::Idle) {
            hide_pill(&app);
        }
    });
}
