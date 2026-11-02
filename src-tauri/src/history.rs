//! What was dictated: kept in memory, and in `history.json` next to the
//! settings when the user wants it. Text only, never audio.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// What was pasted.
    pub text: String,
    /// What whisper heard before any cleanup; equals `text` when nothing changed.
    #[serde(default)]
    pub raw: String,
    /// How `raw` became `text`: "as spoken", "clean", "format · note", "summary", ...
    #[serde(default)]
    pub mode: String,
    pub app: String,
    pub engine: String,
    pub latency_ms: u64,
    /// Unix seconds.
    pub at: u64,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

const MAX_ENTRIES: usize = 500;

pub struct History {
    path: PathBuf,
    entries: VecDeque<HistoryEntry>,
}

impl History {
    /// Loads `path`, dropping entries older than `days` (0 keeps everything).
    pub fn load(path: PathBuf, days: u32) -> Self {
        let mut entries: VecDeque<HistoryEntry> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if days > 0 {
            let cutoff = now().saturating_sub(u64::from(days) * 86_400);
            entries.retain(|e| e.at >= cutoff);
        }
        entries.truncate(MAX_ENTRIES);
        Self { path, entries }
    }

    pub fn entries(&self) -> Vec<HistoryEntry> {
        self.entries.iter().cloned().collect()
    }

    pub fn last(&self) -> Option<&HistoryEntry> {
        self.entries.front()
    }

    /// Newest first. Written to disk only when `persist` is set.
    pub fn push(&mut self, entry: HistoryEntry, persist: bool) {
        self.entries.push_front(entry);
        self.entries.truncate(MAX_ENTRIES);
        if persist {
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.forget_file();
    }

    /// Removes the file without touching what's in memory (history turned off).
    pub fn forget_file(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    fn save(&self) {
        let write = || -> anyhow::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&self.entries)?)?;
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        };
        if let Err(e) = write() {
            log::warn!("couldn't save history: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, text: &str) -> HistoryEntry {
        HistoryEntry {
            text: text.into(),
            raw: text.into(),
            mode: "clean".into(),
            app: String::new(),
            engine: String::new(),
            latency_ms: 0,
            at,
        }
    }

    #[test]
    fn persists_and_prunes() {
        let dir = std::env::temp_dir().join(format!("talyxel-history-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut h = History::load(path.clone(), 14);
        h.push(entry(now() - 30 * 86_400, "old"), true);
        h.push(entry(now(), "new"), true);
        assert_eq!(h.entries().len(), 2);

        let h = History::load(path.clone(), 14);
        assert_eq!(h.entries().iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["new"]);
        // Pruning happens at load and isn't written back; 0 days keeps everything.
        let h = History::load(path.clone(), 0);
        assert_eq!(h.entries().len(), 2);

        let mut h = History::load(path.clone(), 14);
        h.clear();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
