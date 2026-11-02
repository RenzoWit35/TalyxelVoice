//! Background auto-update: check at startup and every few hours, download and
//! verify silently, then let the user restart when it suits them. Never
//! installs mid-dictation.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::pipeline::Dictation;
use crate::AppState;

const FIRST_CHECK: Duration = Duration::from_secs(15);
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading { version: String, percent: u8 },
    Ready { version: String, notes: String },
    Error { message: String },
}

pub struct Updates {
    status: Mutex<Status>,
    /// Verified installer bytes waiting for a restart.
    pending: Mutex<Option<(Update, Vec<u8>)>>,
    /// Tray entry that turns into "Restart to update" once one is ready.
    pub tray_item: Mutex<Option<MenuItem<Wry>>>,
}

impl Default for Updates {
    fn default() -> Self {
        Self { status: Mutex::new(Status::Idle), pending: Mutex::new(None), tray_item: Mutex::new(None) }
    }
}

impl Updates {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

fn set_status(app: &AppHandle, status: Status) {
    let updates = app.state::<Updates>();
    if let Some(item) = updates.tray_item.lock().unwrap().as_ref() {
        let label = match &status {
            Status::Ready { version, .. } => format!("Restart to update to {version}"),
            Status::Checking => "Checking for updates…".into(),
            Status::Downloading { percent, .. } => format!("Downloading update… {percent}%"),
            _ => "Check for updates".into(),
        };
        let _ = item.set_text(label);
        let _ = item.set_enabled(!matches!(status, Status::Checking | Status::Downloading { .. }));
    }
    *updates.status.lock().unwrap() = status.clone();
    let _ = app.emit("update-status", status);
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            check_and_download(&app).await;
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

fn updater(app: &AppHandle) -> tauri_plugin_updater::Result<tauri_plugin_updater::Updater> {
    let mut builder = app.updater_builder();
    // Dev builds can point at a local feed to exercise the whole flow.
    #[cfg(debug_assertions)]
    if let Some(url) = std::env::var("TALYXEL_UPDATE_URL").ok().and_then(|u| u.parse().ok()) {
        builder = builder.endpoints(vec![url])?;
    }
    builder.build()
}

pub async fn check_and_download(app: &AppHandle) {
    let current = app.state::<Updates>().status();
    if matches!(current, Status::Ready { .. } | Status::Checking | Status::Downloading { .. }) {
        return;
    }
    set_status(app, Status::Checking);
    match fetch(app).await {
        Ok(status) => set_status(app, status),
        Err(e) => {
            log::warn!("update check failed: {e}");
            set_status(app, Status::Error { message: e.to_string() });
        }
    }
}

async fn fetch(app: &AppHandle) -> tauri_plugin_updater::Result<Status> {
    let Some(update) = updater(app)?.check().await? else {
        return Ok(Status::UpToDate);
    };
    let version = update.version.clone();
    log::info!("update {version} available, downloading");

    let mut received = 0usize;
    let mut last_percent = 0u8;
    let bytes = update
        .download(
            |chunk, total| {
                received += chunk;
                let percent = total.map_or(0, |t| (received as u64 * 100 / t.max(1)) as u8);
                if percent != last_percent {
                    last_percent = percent;
                    set_status(app, Status::Downloading { version: version.clone(), percent });
                }
            },
            || {},
        )
        .await?;

    let notes = update.body.clone().unwrap_or_default();
    *app.state::<Updates>().pending.lock().unwrap() = Some((update, bytes));
    log::info!("update {version} downloaded and verified");
    Ok(Status::Ready { version, notes })
}

/// Runs the installer, which exits the app on Windows and relaunches it.
pub fn install(app: &AppHandle) -> Result<(), String> {
    if !matches!(*app.state::<AppState>().dictation.lock().unwrap(), Dictation::Idle) {
        return Err("Finish the current dictation first".into());
    }
    let pending = app.state::<Updates>().pending.lock().unwrap().take();
    let Some((update, bytes)) = pending else {
        return Err("No update is ready".into());
    };
    log::info!("installing update {}", update.version);
    update.install(bytes).map_err(|e| e.to_string())?;
    // macOS/Linux install in place; restart to load the new version.
    app.restart();
}
