use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::audio::{self, WHISPER_RATE};
use crate::settings::{Engine, RemoteSettings, Settings};

pub struct Request {
    pub samples: Vec<f32>,
    pub language: Option<String>,
    pub prompt: String,
}

/// Whisper's text plus the language it heard (set or detected), as a code
/// like "nl"; the text model needs it to avoid translating.
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

impl Request {
    pub fn new(samples: Vec<f32>, settings: &Settings) -> Self {
        Self {
            samples,
            language: settings.language_code().map(str::to_owned),
            prompt: settings.initial_prompt(),
        }
    }
}

/// Keeps the loaded model and its decoder state between dictations: loading
/// takes longer than transcribing a sentence, and a fresh state's first run is
/// several times slower than later ones.
#[derive(Default)]
pub struct LocalWhisper {
    /// The model, whether it's on the GPU, and its state.
    loaded: Mutex<Option<(PathBuf, bool, WhisperState)>>,
    /// Settings' `gpu`; a change reloads the model on the next dictation.
    use_gpu: AtomicBool,
}

impl LocalWhisper {
    pub fn set_gpu(&self, on: bool) {
        self.use_gpu.store(on, Ordering::Relaxed);
    }

    /// Loads `model` into `guard` unless it's already there on the right device.
    fn ensure_loaded(&self, guard: &mut Option<(PathBuf, bool, WhisperState)>, model: &Path) -> anyhow::Result<()> {
        let gpu = crate::gpu::pick().filter(|_| self.use_gpu.load(Ordering::Relaxed));
        if guard.as_ref().map(|(p, g, _)| (p.as_path(), *g)) != Some((model, gpu.is_some())) {
            *guard = None;
            if !model.exists() {
                bail!("model {} isn't downloaded yet — get it in Settings", model.display());
            }
            let mut ctx_params = WhisperContextParameters::default();
            ctx_params.flash_attn(true);
            ctx_params.use_gpu(gpu.is_some());
            if let Some(gpu) = gpu {
                ctx_params.gpu_device(gpu.index);
            }
            let ctx = WhisperContext::new_with_params(
                model.to_str().context("model path isn't UTF-8")?,
                ctx_params,
            )
            .map_err(|e| anyhow!("failed to load model: {e}"))?;
            // The state keeps the context alive.
            let state = ctx.create_state().map_err(|e| anyhow!("{e}"))?;
            *guard = Some((model.to_owned(), gpu.is_some(), state));
        }
        Ok(())
    }

    pub fn transcribe(&self, model: &Path, req: &Request) -> anyhow::Result<Transcript> {
        let mut guard = self.loaded.lock().unwrap();
        self.ensure_loaded(&mut guard, model)?;
        let (_, _, state) = guard.as_mut().unwrap();

        let mut params = base_params(req);
        params.set_no_timestamps(true);

        // whisper.cpp refuses input under one second.
        let min = WHISPER_RATE as usize + WHISPER_RATE as usize / 10;
        let mut samples = req.samples.clone();
        if samples.len() < min {
            samples.resize(min, 0.0);
        }
        params.set_audio_ctx(audio_ctx(samples.len()));
        state.full(params, &samples).map_err(|e| anyhow!("whisper failed: {e}"))?;

        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(&segment.to_str_lossy().map_err(|e| anyhow!("{e}"))?);
        }
        let language = req.language.clone().or_else(|| {
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_owned)
        });
        Ok(Transcript { text: clean(&text), language })
    }

    /// A whole recording, minutes or hours long, with a time range per
    /// segment. `progress` gets whisper's percentage; returning true from
    /// `abort` stops it mid-file.
    pub fn transcribe_file(
        &self,
        model: &Path,
        req: &Request,
        progress: impl FnMut(i32) + 'static,
        abort: impl FnMut() -> bool + 'static,
    ) -> anyhow::Result<(Vec<Segment>, Option<String>)> {
        let mut guard = self.loaded.lock().unwrap();
        self.ensure_loaded(&mut guard, model)?;
        let (_, _, state) = guard.as_mut().unwrap();

        // Unlike dictation, the encoder keeps its full 30 s window (no
        // `set_audio_ctx`): recordings are long, and timestamps need it.
        let mut params = base_params(req);
        params.set_progress_callback_safe(progress);
        // whisper-rs 0.16 boxes this closure as `Box<dyn FnMut>` but its
        // trampoline reads it back as `F`; only `F = Box<dyn FnMut>` matches.
        params.set_abort_callback_safe(Box::new(abort) as Box<dyn FnMut() -> bool>);
        let min = WHISPER_RATE as usize + WHISPER_RATE as usize / 10;
        let mut samples = req.samples.clone();
        if samples.len() < min {
            samples.resize(min, 0.0);
        }
        state.full(params, &samples).map_err(|e| anyhow!("whisper failed: {e}"))?;

        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = clean(&segment.to_str_lossy().map_err(|e| anyhow!("{e}"))?);
            if text.is_empty() {
                continue;
            }
            // whisper counts in centiseconds.
            segments.push(Segment {
                start_ms: segment.start_timestamp().max(0) as u64 * 10,
                end_ms: segment.end_timestamp().max(0) as u64 * 10,
                text,
            });
        }
        let language = req.language.clone().or_else(|| {
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_owned)
        });
        Ok((segments, language))
    }

    /// Loads the model and runs it once on silence so the first real
    /// dictation doesn't pay for allocation and cache warm-up.
    pub fn warm_up(&self, model: &Path) {
        if !model.exists() {
            return;
        }
        let silence = Request {
            samples: vec![0.0; WHISPER_RATE as usize * 2],
            language: Some("en".into()),
            prompt: String::new(),
        };
        let started = std::time::Instant::now();
        match self.transcribe(model, &silence) {
            Ok(_) => log::info!("whisper warmed up in {:?}", started.elapsed()),
            Err(e) => log::warn!("whisper warm-up failed: {e}"),
        }
    }
}

/// One stretch of a recording as whisper split it, roughly a sentence.
#[derive(Debug, Clone)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

fn base_params(req: &Request) -> FullParams<'_, '_> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(req.language.as_deref().unwrap_or("auto")));
    if !req.prompt.is_empty() {
        params.set_initial_prompt(&req.prompt);
    }
    params.set_n_threads(default_threads() as i32);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params
}

/// The encoder always runs over a 30 s window (1500 frames, 50 per second)
/// unless told otherwise; dictations are a few seconds, so size it to the
/// clip plus a second of slack. This is most of the local speed.
fn audio_ctx(samples: usize) -> i32 {
    let secs = samples as f32 / WHISPER_RATE as f32;
    let frames = (secs * 50.0) as i32 + 50;
    (((frames + 63) / 64) * 64).min(1500)
}

/// All but two logical cores: measurably faster than 8 on hybrid Intel parts,
/// and leaves room for the app the user is typing into.
fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(2).clamp(1, 16))
}

fn transcriptions_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/audio/transcriptions") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/audio/transcriptions")
    } else {
        format!("{base}/v1/audio/transcriptions")
    }
}

#[derive(serde::Deserialize)]
struct RemoteResponse {
    text: String,
}

pub async fn transcribe_remote(cfg: &RemoteSettings, req: &Request) -> anyhow::Result<String> {
    let wav = audio::to_wav(&req.samples)?;
    let file = reqwest::multipart::Part::bytes(wav)
        .file_name("dictation.wav")
        .mime_str("audio/wav")?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("model", cfg.model.clone())
        .text("response_format", "json")
        .text("temperature", "0");
    if let Some(lang) = &req.language {
        form = form.text("language", lang.clone());
    }
    if !req.prompt.is_empty() {
        form = form.text("prompt", req.prompt.clone());
    }

    let mut request = reqwest::Client::new()
        .post(transcriptions_url(&cfg.url))
        .timeout(std::time::Duration::from_secs(60))
        .multipart(form);
    if !cfg.api_key.is_empty() {
        request = request.bearer_auth(&cfg.api_key);
    }
    let resp = request.send().await.context("couldn't reach transcription server")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("server returned {status}: {}", body.chars().take(300).collect::<String>());
    }
    let parsed: RemoteResponse = resp.json().await.context("unexpected server response")?;
    Ok(clean(&parsed.text))
}

/// Sends a short silent clip so the check exercises the same path as a real
/// dictation, rather than relying on a `/models` route not every server has.
pub async fn test_remote(cfg: &RemoteSettings) -> anyhow::Result<String> {
    let req = Request {
        samples: vec![0.0; WHISPER_RATE as usize],
        language: None,
        prompt: String::new(),
    };
    transcribe_remote(cfg, &req).await?;
    Ok(format!("Connected to {}", transcriptions_url(&cfg.url)))
}

pub async fn transcribe(
    settings: &Settings,
    local: Arc<LocalWhisper>,
    models_dir: &Path,
    req: Request,
) -> anyhow::Result<Transcript> {
    match settings.engine {
        Engine::Remote => {
            let text = transcribe_remote(&settings.remote, &req).await?;
            Ok(Transcript { text, language: req.language })
        }
        Engine::Local => {
            let model = crate::settings::model_path(models_dir, &settings.local_model);
            // whisper is CPU-bound; keep it off the async workers.
            tauri::async_runtime::spawn_blocking(move || local.transcribe(&model, &req)).await?
        }
    }
}

/// Whisper hallucinates these on silence or breaths.
const NOISE: &[&str] = &["[BLANK_AUDIO]", "[Music]", "(silence)", "[ Silence ]", "[silence]"];

fn clean(text: &str) -> String {
    let mut t = text.to_owned();
    for n in NOISE {
        t = t.replace(n, "");
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_forms() {
        assert_eq!(transcriptions_url("http://h:8000"), "http://h:8000/v1/audio/transcriptions");
        assert_eq!(transcriptions_url("http://h:8000/v1/"), "http://h:8000/v1/audio/transcriptions");
        assert_eq!(
            transcriptions_url("https://h/v1/audio/transcriptions"),
            "https://h/v1/audio/transcriptions"
        );
    }

    #[test]
    fn strips_noise_tokens() {
        assert_eq!(clean(" [BLANK_AUDIO]  hello   world "), "hello world");
    }

    /// `TALYXEL_TEST_REMOTE=http://host:port cargo test -- --ignored`
    #[test]
    #[ignore]
    fn remote_end_to_end() {
        let url = std::env::var("TALYXEL_TEST_REMOTE").expect("TALYXEL_TEST_REMOTE");
        let cfg = RemoteSettings { url, api_key: "test-key".into(), ..Default::default() };
        let mut settings = Settings::default();
        settings.language = "en".into();
        settings.dictionary = vec!["Talyxel".into()];
        let req = Request::new(vec![0.1; WHISPER_RATE as usize * 2], &settings);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let text = rt.block_on(transcribe_remote(&cfg, &req)).unwrap();
        assert_eq!(text, "remote server says hello");
        println!("{}", rt.block_on(test_remote(&cfg)).unwrap());
    }

    /// `TALYXEL_TEST_MODEL=... TALYXEL_TEST_WAV=... cargo test -- --ignored`
    /// with a 16 kHz mono 16-bit WAV.
    #[test]
    #[ignore]
    fn local_whisper_end_to_end() {
        let model = std::env::var("TALYXEL_TEST_MODEL").expect("TALYXEL_TEST_MODEL");
        let wav = std::env::var("TALYXEL_TEST_WAV").expect("TALYXEL_TEST_WAV");
        let samples: Vec<f32> = hound::WavReader::open(wav)
            .unwrap()
            .into_samples::<i16>()
            .map(|s| s.unwrap() as f32 / i16::MAX as f32)
            .collect();
        let mut settings = Settings::default();
        settings.dictionary = vec!["Talyxel".into()];
        let whisper = LocalWhisper::default();
        whisper.warm_up(Path::new(&model));
        let mut text = String::new();
        for language in ["auto", "en"] {
            settings.language = language.into();
            let req = Request::new(samples.clone(), &settings);
            let started = std::time::Instant::now();
            text = whisper.transcribe(Path::new(&model), &req).unwrap().text;
            println!("[{language}: {:?}] {text}", started.elapsed());
        }
        assert!(text.to_lowercase().contains("pull request"), "got: {text}");
    }
}
