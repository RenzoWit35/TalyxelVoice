//! Notes: a place to dictate into that isn't someone else's text field.
//! Kept in `notes.json` next to the settings; text only.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::history;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Note {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
}

pub struct Notes {
    path: PathBuf,
    notes: Vec<Note>,
}

impl Notes {
    pub fn load(path: PathBuf) -> Self {
        let notes = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, notes }
    }

    pub fn all(&self) -> Vec<Note> {
        self.notes.clone()
    }

    pub fn get(&self, id: u64) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    /// Inserts or updates; a note with id 0 is new.
    pub fn save(&mut self, mut note: Note) -> Note {
        let now = history::now();
        note.updated = now;
        if note.id == 0 {
            note.id = self.new_id();
            note.created = now;
            self.notes.push(note.clone());
        } else if let Some(existing) = self.notes.iter_mut().find(|n| n.id == note.id) {
            note.created = existing.created;
            *existing = note.clone();
        } else {
            self.notes.push(note.clone());
        }
        self.persist();
        note
    }

    /// Adds `text` as a new paragraph; creates the note when `id` is unknown.
    pub fn append(&mut self, id: u64, text: &str) -> Note {
        let mut note = self.get(id).cloned().unwrap_or_default();
        let body = note.body.trim_end();
        note.body = if body.is_empty() { text.to_owned() } else { format!("{body}\n\n{text}") };
        self.save(note)
    }

    pub fn delete(&mut self, id: u64) {
        self.notes.retain(|n| n.id != id);
        self.persist();
    }

    fn new_id(&self) -> u64 {
        let base = history::now() * 1000;
        (base..).find(|id| self.get(*id).is_none()).unwrap_or(base)
    }

    fn persist(&self) {
        let write = || -> anyhow::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&self.notes)?)?;
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        };
        if let Err(e) = write() {
            log::warn!("couldn't save notes: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_appends() {
        let dir = std::env::temp_dir().join(format!("talyxel-notes-{}", std::process::id()));
        let mut notes = Notes::load(dir.join("notes.json"));
        let note = notes.save(Note { body: "Gebeld met Jan over de printer, toner bestellen".into(), ..Default::default() });
        assert!(note.id > 0);

        let note = notes.append(note.id, "Toner is besteld.");
        assert!(note.body.ends_with("\n\nToner is besteld."));
        let fresh = notes.append(0, "Nieuw");
        assert_ne!(fresh.id, note.id);
        assert_eq!(fresh.body, "Nieuw");

        let reloaded = Notes::load(dir.join("notes.json"));
        assert_eq!(reloaded.all().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
