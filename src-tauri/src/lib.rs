mod audio;
mod history;
mod pipeline;
mod platform;
mod polish;
mod settings;
mod stt;
mod updates;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use history::{History, HistoryEntry};
use pipeline::{Dictation, Kind};
use settings::{RemoteSettings, Settings};

pub struct AppState {
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    models_dir: PathBuf,
    local: Arc<stt::LocalWhisper>,
    text_model: Arc<polish::LocalTextModel>,
    dictation: Mutex<Dictation>,
    history: Mutex<History>,
    /// Registered shortcut id → what it does.
    hotkeys: Mutex<HashMap<u32, Action>>,
}

/// What a global hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Dictate,
    Summary,
    Undo,
    Repaste,
}

/// (name, approximate size in MB) from ggerganov/whisper.cpp on Hugging Face.
const MODELS: &[(&str, u32)] = &[
    ("tiny", 75),
    ("base", 142),
    ("small", 466),
    ("small-q5_1", 190),
    ("medium-q5_0", 514),
    ("large-v3-turbo-q5_0", 547),
];

/// (name, approximate size in MB, GGUF URL) for formatting and summaries.
/// Picked by testing on Dutch/English dictations: the smallest model that
/// keeps the speaker's language instead of translating to English.
const TEXT_MODELS: &[(&str, u32, &str)] = &[(
    "qwen2.5-1.5b",
    1066,
    "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf",
)];

#[derive(Serialize)]
struct ModelInfo {
    name: &'static str,
    size_mb: u32,
    downloaded: bool,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> CmdResult<()> {
    let hotkeys = register_hotkeys(&app, &settings)?;
    *state.hotkeys.lock().unwrap() = hotkeys;
    settings.save(&state.settings_path).map_err(err)?;
    let previous = std::mem::replace(&mut *state.settings.lock().unwrap(), settings.clone());
    if previous.engine != settings.engine || previous.local_model != settings.local_model {
        warm_up(&app);
    }
    if previous.keep_history && !settings.keep_history {
        state.history.lock().unwrap().forget_file();
    }
    Ok(())
}

/// Preloads the local model off-thread so the first dictation is fast.
fn warm_up(app: &AppHandle) {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    if settings.engine != settings::Engine::Local {
        return;
    }
    let model = settings::model_path(&state.models_dir, &settings.local_model);
    let local = state.local.clone();
    tauri::async_runtime::spawn_blocking(move || local.warm_up(&model));
}

#[tauri::command]
fn list_microphones() -> Vec<String> {
    audio::input_devices()
}

#[tauri::command]
fn list_models(state: State<AppState>) -> Vec<ModelInfo> {
    MODELS
        .iter()
        .map(|&(name, size_mb)| ModelInfo {
            name,
            size_mb,
            downloaded: settings::model_path(&state.models_dir, name).exists(),
        })
        .collect()
}

#[tauri::command]
fn list_text_models(state: State<AppState>) -> Vec<ModelInfo> {
    TEXT_MODELS
        .iter()
        .map(|&(name, size_mb, _)| ModelInfo {
            name,
            size_mb,
            downloaded: settings::text_model_path(&state.models_dir, name).exists(),
        })
        .collect()
}

#[derive(Clone, Serialize)]
struct Progress {
    name: String,
    downloaded: u64,
    total: u64,
}

/// Downloads a whisper model, or a text model when `text` is set.
#[tauri::command]
async fn download_model(app: AppHandle, name: String, text: Option<bool>) -> CmdResult<()> {
    let dir = app.state::<AppState>().models_dir.clone();
    let (url, final_path) = if text.unwrap_or(false) {
        let (_, _, url) = TEXT_MODELS
            .iter()
            .find(|(n, ..)| *n == name)
            .ok_or_else(|| format!("unknown model {name}"))?;
        (url.to_string(), settings::text_model_path(&dir, &name))
    } else {
        if !MODELS.iter().any(|(n, _)| *n == name) {
            return Err(format!("unknown model {name}"));
        }
        let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin");
        (url, settings::model_path(&dir, &name))
    };
    std::fs::create_dir_all(&dir).map_err(err)?;
    let resp = reqwest::get(&url).await.map_err(err)?.error_for_status().map_err(err)?;
    let total = resp.content_length().unwrap_or(0);
    let part = final_path.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(err)?;
    let mut downloaded = 0u64;
    let mut last_emit = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(err)?;
        std::io::Write::write_all(&mut file, &chunk).map_err(err)?;
        downloaded += chunk.len() as u64;
        if downloaded - last_emit > 2 * 1024 * 1024 || downloaded == total {
            last_emit = downloaded;
            let _ = app.emit("model-progress", Progress { name: name.clone(), downloaded, total });
        }
    }
    drop(file);
    std::fs::rename(&part, &final_path).map_err(err)?;
    Ok(())
}

#[tauri::command]
async fn test_remote(remote: RemoteSettings) -> CmdResult<String> {
    stt::test_remote(&remote).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn test_text_remote(remote: RemoteSettings) -> CmdResult<String> {
    polish::test_remote(&remote).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn get_history(state: State<AppState>) -> Vec<HistoryEntry> {
    state.history.lock().unwrap().entries()
}

#[tauri::command]
fn clear_history(state: State<AppState>) {
    state.history.lock().unwrap().clear();
}

fn parse_hotkey(hotkey: &str) -> CmdResult<Shortcut> {
    hotkey.parse().map_err(|e| format!("invalid hotkey {hotkey:?}: {e}"))
}

/// Registers every configured hotkey (an empty one is off) and returns the
/// shortcut id → action map the handler looks up.
fn register_hotkeys(app: &AppHandle, settings: &Settings) -> CmdResult<HashMap<u32, Action>> {
    let wanted = [
        (Action::Dictate, settings.hotkey.as_str()),
        (Action::Summary, settings.summary_hotkey.as_str()),
        (Action::Undo, settings.undo_hotkey.as_str()),
        (Action::Repaste, settings.repaste_hotkey.as_str()),
    ];
    let mut map = HashMap::new();
    let mut shortcuts = Vec::new();
    for (action, hotkey) in wanted {
        if hotkey.trim().is_empty() {
            if action == Action::Dictate {
                return Err("the dictation hotkey can't be empty".into());
            }
            continue;
        }
        let shortcut = parse_hotkey(hotkey)?;
        if map.insert(shortcut.id(), action).is_some() {
            return Err(format!("{hotkey} is used for two things; pick a different key"));
        }
        shortcuts.push((shortcut, hotkey));
    }
    let gs = app.global_shortcut();
    gs.unregister_all().map_err(err)?;
    for (shortcut, name) in shortcuts {
        gs.register(shortcut).map_err(|e| format!("couldn't register {name}: {e}"))?;
    }
    Ok(map)
}

fn on_shortcut(app: &AppHandle, shortcut: &Shortcut, pressed: bool) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let action = state.hotkeys.lock().unwrap().get(&shortcut.id()).copied();
    match action {
        Some(Action::Dictate) => pipeline::on_hotkey(app, pressed, Kind::Dictate),
        Some(Action::Summary) => pipeline::on_hotkey(app, pressed, Kind::Summary),
        Some(Action::Undo) if pressed => pipeline::paste_last(app, true),
        Some(Action::Repaste) if pressed => pipeline::paste_last(app, false),
        _ => {}
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
fn get_update_status(updates: State<updates::Updates>) -> updates::Status {
    updates.status()
}

#[tauri::command]
async fn check_for_updates(app: AppHandle) {
    updates::check_and_download(&app).await;
}

#[tauri::command]
fn install_update(app: AppHandle) -> CmdResult<()> {
    updates::install(&app)
}

fn on_update_menu(app: &AppHandle) {
    if matches!(app.state::<updates::Updates>().status(), updates::Status::Ready { .. }) {
        if let Err(e) = updates::install(app) {
            log::warn!("{e}");
        }
    } else {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { updates::check_and_download(&app).await });
    }
}

fn place_pill(app: &AppHandle) -> tauri::Result<()> {
    let Some(pill) = app.get_webview_window("pill") else { return Ok(()) };
    if let Ok(hwnd) = pill.hwnd() {
        platform::make_overlay(hwnd.0 as _);
    }
    if let Some(monitor) = pill.primary_monitor()? {
        let area = monitor.work_area();
        let size = pill.outer_size()?;
        let margin = (24.0 * monitor.scale_factor()) as i32;
        pill.set_position(PhysicalPosition::new(
            area.position.x + (area.size.width as i32 - size.width as i32) / 2,
            area.position.y + area.size.height as i32 - size.height as i32 - margin,
        ))?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Stdout plus %LOCALAPPDATA%\com.talyxel.voice\logs.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::Updates::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    on_shortcut(app, shortcut, event.state() == ShortcutState::Pressed);
                })
                .build(),
        )
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let settings_path = config_dir.join("settings.json");
            let settings = Settings::load(&settings_path);

            let hotkeys = register_hotkeys(app.handle(), &settings).unwrap_or_else(|e| {
                log::error!("{e}");
                HashMap::new()
            });
            let history_days = if settings.keep_history { settings.history_days } else { 0 };
            let history = History::load(config_dir.join("history.json"), history_days);
            if !settings.keep_history {
                history.forget_file();
            }
            let llama_server = app.path().resource_dir()?.join("llama").join("llama-server.exe");
            app.manage(AppState {
                settings: Mutex::new(settings),
                settings_path,
                models_dir: data_dir.join("models"),
                local: Arc::new(stt::LocalWhisper::default()),
                text_model: polish::LocalTextModel::new(llama_server),
                dictation: Mutex::new(Dictation::Idle),
                history: Mutex::new(history),
                hotkeys: Mutex::new(hotkeys),
            });
            // Created here rather than from the config, so its first invoke
            // can't run before AppState is managed (the bundled page loads fast).
            if let Some(main) = app.config().app.windows.iter().find(|w| w.label == "main") {
                WebviewWindowBuilder::from_config(app, main)?.build()?;
            }

            warm_up(app.handle());
            place_pill(app.handle())?;

            let open = MenuItem::with_id(app, "open", "Open TalyxelVoice", true, None::<&str>)?;
            let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &update, &quit])?;
            *app.state::<updates::Updates>().tray_item.lock().unwrap() = Some(update);
            updates::start(app.handle().clone());
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("TalyxelVoice")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "update" => on_update_menu(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window keeps the app alive in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_microphones,
            list_models,
            list_text_models,
            download_model,
            test_text_remote,
            test_remote,
            get_history,
            clear_history,
            app_version,
            get_update_status,
            check_for_updates,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TalyxelVoice");
}
