use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// whisper.cpp running in-process.
    Local,
    /// Any OpenAI-compatible `/v1/audio/transcriptions` server (speaches,
    /// faster-whisper-server, whisper.cpp `server`, ...).
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSettings {
    pub url: String,
    pub api_key: String,
    pub model: String,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            url: "http://localhost:8000".into(),
            api_key: String::new(),
            model: "Systran/faster-whisper-large-v3".into(),
        }
    }
}

/// What happens to every dictation after transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Polish {
    /// Paste exactly what whisper heard.
    Off,
    /// Drop filler words and stutters; rule-based, instant, no model.
    Clean,
    /// Clean, then let the text model add paragraphs and lists.
    Format,
}

/// The shape `Polish::Format` gives the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatStyle {
    /// Paragraphs, with lists where the speaker enumerates.
    Text,
    /// A terse note: just the facts.
    Note,
    /// Everything as bullets.
    List,
    /// Time spent and subject, then what was done (for hour logs and tickets).
    Timelog,
}

/// The model that formats and summarizes: llama-server bundled with the app,
/// or any OpenAI-compatible `/v1/chat/completions` server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TextModelSettings {
    pub engine: Engine,
    /// GGUF name, stored as `models/llm-<name>.gguf`.
    pub local_model: String,
    pub remote: RemoteSettings,
}

impl Default for TextModelSettings {
    fn default() -> Self {
        Self {
            engine: Engine::Local,
            local_model: "qwen2.5-1.5b".into(),
            remote: RemoteSettings {
                url: "http://localhost:8080".into(),
                api_key: String::new(),
                model: String::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Hold to dictate; tap once for hands-free, tap again to stop.
    pub hotkey: String,
    /// Same as `hotkey`, but pastes a summary of what you said.
    pub summary_hotkey: String,
    pub polish: Polish,
    pub format_style: FormatStyle,
    pub text_model: TextModelSettings,
    pub engine: Engine,
    /// ggml model name, stored as `models/ggml-<name>.bin`.
    pub local_model: String,
    pub remote: RemoteSettings,
    /// Whisper language code, or "auto".
    pub language: String,
    /// Languages you mix while speaking; steers whisper toward code-switching.
    pub spoken_languages: Vec<String>,
    /// Names and jargon whisper should spell correctly.
    pub dictionary: Vec<String>,
    /// Input device name; empty means the system default.
    pub microphone: String,
    pub restore_clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Shift+Space".into(),
            summary_hotkey: "Alt+Shift+Space".into(),
            polish: Polish::Clean,
            format_style: FormatStyle::Text,
            text_model: TextModelSettings::default(),
            engine: Engine::Local,
            local_model: "small-q5_1".into(),
            remote: RemoteSettings::default(),
            language: "auto".into(),
            spoken_languages: Vec::new(),
            dictionary: Vec::new(),
            microphone: String::new(),
            restore_clipboard: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Initial prompt shared by both engines: vocabulary plus a hint at the
    /// languages being mixed, which is what nudges whisper to keep each
    /// phrase in its own language instead of translating.
    pub fn initial_prompt(&self) -> String {
        let mut parts = Vec::new();
        if self.spoken_languages.len() > 1 {
            parts.push(format!(
                "The speaker switches between {} mid-sentence.",
                self.spoken_languages.join(" and ")
            ));
        }
        if !self.dictionary.is_empty() {
            parts.push(format!("Vocabulary: {}.", self.dictionary.join(", ")));
        }
        parts.join(" ")
    }

    pub fn language_code(&self) -> Option<&str> {
        match self.language.trim() {
            "" | "auto" => None,
            code => Some(code),
        }
    }
}

pub fn model_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(format!("ggml-{name}.bin"))
}

pub fn text_model_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(format!("llm-{name}.gguf"))
}
