//! After transcription: rule-based cleanup, and the text model that formats
//! or summarizes. The model is llama.cpp's `llama-server` (bundled, started on
//! demand, stopped when idle) or any OpenAI-compatible chat endpoint.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::settings::{Engine, FormatStyle, RemoteSettings, Snippet, TextModelSettings};

// --- rule-based cleanup ---------------------------------------------------

/// Hesitations whisper sometimes writes out. Real words are left alone, so
/// "um" stays in German, where it means "around".
const FILLERS: &[&str] = &["uh", "uhh", "uhm", "um", "umm", "erm", "eh", "ehm", "euh", "hmm", "mm", "öh", "äh", "ähm"];
/// Words that legitimately appear twice in a row ("ik denk dat dat klopt").
const KEEP_DOUBLE: &[&str] = &["dat", "die", "had", "that", "is", "was", "er", "de", "der", "das"];

fn core(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

fn ends_sentence(word: &str) -> bool {
    word.ends_with(['.', '!', '?'])
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Drops filler words and stutters ("we should, we should go"), keeping the
/// punctuation and capitalization whisper produced around them.
pub fn strip_fillers(text: &str, language: Option<&str>) -> String {
    let german = language == Some("de");
    let mut words: Vec<String> = Vec::new();
    // Whether the next kept word starts a sentence, e.g. after "Eh, ".
    let mut sentence_start = true;
    let mut capitalize_next = false;
    for word in text.split_whitespace() {
        let c = core(word);
        if FILLERS.contains(&c.as_str()) && !(german && c == "um") {
            if ends_sentence(word) {
                // "klaar eh." → "klaar."
                if let Some(prev) = words.last_mut() {
                    let trimmed = prev.trim_end_matches([',', ';']).to_owned();
                    *prev = trimmed + &word[word.len() - 1..];
                }
                sentence_start = true;
            } else if sentence_start {
                capitalize_next = true;
            }
            continue;
        }
        let mut w = word.to_owned();
        if capitalize_next {
            w = capitalize(&w);
            capitalize_next = false;
        }
        sentence_start = ends_sentence(&w);
        words.push(w);
    }

    // Collapse a phrase said twice in a row, longest first; keep the second
    // copy, which carries the sentence on.
    for n in (1..=3).rev() {
        let mut i = 0;
        while i + 2 * n <= words.len() {
            let first: Vec<String> = words[i..i + n].iter().map(|w| core(w)).collect();
            let second: Vec<String> = words[i + n..i + 2 * n].iter().map(|w| core(w)).collect();
            let repeated = first == second
                && first.iter().all(|w| !w.is_empty())
                && !(n == 1 && KEEP_DOUBLE.contains(&first[0].as_str()))
                // "Klaar. Klaar." is two sentences, not a stutter.
                && !words[i + n - 1].ends_with(['.', '!', '?']);
            if repeated {
                let was_capitalized = words[i].chars().next().is_some_and(char::is_uppercase);
                words.drain(i..i + n);
                if was_capitalized {
                    words[i] = capitalize(&words[i]);
                }
            } else {
                i += 1;
            }
        }
    }
    words.join(" ")
}

/// Applies the dictionary's `heard => wanted` fixes: case-insensitive, on
/// whole words only, so "VLAN" doesn't fire inside "vlanen".
pub fn replace_words(text: &str, pairs: &[(String, String)]) -> String {
    let mut out: Vec<char> = text.chars().collect();
    for (from, to) in pairs {
        let from: Vec<char> = from.chars().collect();
        if from.is_empty() {
            continue;
        }
        let boundary = |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric());
        let mut i = 0;
        while i + from.len() <= out.len() {
            let matches = out[i..i + from.len()]
                .iter()
                .zip(&from)
                .all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
                && boundary(if i == 0 { None } else { out.get(i - 1) })
                && boundary(out.get(i + from.len()));
            if matches {
                let replacement: Vec<char> = to.chars().collect();
                let n = replacement.len();
                out.splice(i..i + from.len(), replacement);
                i += n;
            } else {
                i += 1;
            }
        }
    }
    out.into_iter().collect()
}

/// Spoken punctuation, matched in any supported language regardless of the
/// one being dictated, since people mix them. Longest phrase first.
const SPOKEN: &[(&str, &str)] = &[
    ("new paragraph", "\n\n"),
    ("nieuwe alinea", "\n\n"),
    ("nieuwe paragraaf", "\n\n"),
    ("neuer absatz", "\n\n"),
    ("nouveau paragraphe", "\n\n"),
    ("new line", "\n"),
    ("newline", "\n"),
    ("nieuwe regel", "\n"),
    ("neue zeile", "\n"),
    ("nouvelle ligne", "\n"),
    ("question mark", "?"),
    ("vraagteken", "?"),
    ("fragezeichen", "?"),
    ("exclamation mark", "!"),
    ("exclamation point", "!"),
    ("uitroepteken", "!"),
    ("ausrufezeichen", "!"),
    ("full stop", "."),
    ("period", "."),
    ("punt", "."),
    ("punkt", "."),
    ("dubbele punt", ":"),
    ("doppelpunkt", ":"),
    ("colon", ":"),
    ("semicolon", ";"),
    ("puntkomma", ";"),
    ("semikolon", ";"),
    ("comma", ","),
    ("komma", ","),
    ("virgule", ","),
];

/// Turns "we moeten nog bellen komma mailen punt nieuwe regel groet" into
/// "we moeten nog bellen, mailen.\ngroet": the named punctuation attaches to
/// the previous word, and the sentence after a full stop starts capitalized.
pub fn spoken_punctuation(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let phrases: Vec<(Vec<&str>, &str)> =
        SPOKEN.iter().map(|(p, s)| (p.split(' ').collect(), *s)).collect();
    let mut out: Vec<String> = Vec::new();
    let mut capitalize_next = false;
    let mut i = 0;
    while i < words.len() {
        let hit = phrases.iter().find(|(phrase, _)| {
            i + phrase.len() <= words.len()
                && phrase.iter().zip(&words[i..]).all(|(p, w)| core(w) == *p)
        });
        let Some((phrase, symbol)) = hit else {
            let mut w = words[i].to_owned();
            if capitalize_next {
                w = capitalize(&w);
                capitalize_next = false;
            }
            out.push(w);
            i += 1;
            continue;
        };
        i += phrase.len();
        if symbol.starts_with('\n') {
            if let Some(prev) = out.last_mut() {
                if !prev.starts_with('\n') {
                    *prev = prev.trim_end_matches([',', ';', ':']).to_owned();
                }
            }
            if !out.is_empty() {
                out.push((*symbol).to_owned());
            }
            capitalize_next = true;
        } else if let Some(prev) = out.last_mut() {
            if prev.starts_with('\n') {
                continue;
            }
            let base = prev.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != ')');
            *prev = format!("{base}{symbol}");
            capitalize_next = matches!(*symbol, "." | "?" | "!");
        }
    }
    let mut s = String::new();
    for token in out {
        if token.starts_with('\n') {
            s.push_str(&token);
        } else {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push(' ');
            }
            s.push_str(&token);
        }
    }
    s
}

fn normalize(text: &str) -> String {
    text.split_whitespace().map(core).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A snippet trigger said on its own expands to its text; said at the end of
/// a sentence ("thanks for the update, my signature"), the text follows what
/// came before.
pub fn apply_snippet(text: &str, snippets: &[Snippet]) -> Option<String> {
    let said = normalize(text);
    if said.is_empty() {
        return None;
    }
    for snippet in snippets {
        let trigger = normalize(&snippet.trigger);
        if trigger.is_empty() {
            continue;
        }
        if said == trigger {
            return Some(snippet.text.clone());
        }
        let Some(prefix) = said.strip_suffix(&trigger) else { continue };
        if !prefix.ends_with(' ') {
            continue;
        }
        // Drop the trigger's words from the end of the original text.
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut remaining = trigger.split(' ').count();
        let mut keep = words.len();
        while remaining > 0 && keep > 0 {
            keep -= 1;
            if !core(words[keep]).is_empty() {
                remaining -= 1;
            }
        }
        let before = words[..keep].join(" ");
        let before = before.trim_end_matches([',', ';', ':', ' ']);
        let glue = if snippet.text.contains('\n') { "\n\n" } else { " " };
        return Some(format!("{before}{glue}{}", snippet.text));
    }
    None
}

// --- the text model --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Format(FormatStyle),
    Summarize,
}

/// A bundled llama-server, started when first needed and stopped after
/// `IDLE` so a 1-2 GB model doesn't sit in RAM between dictations.
pub struct LocalTextModel {
    exe: PathBuf,
    server: tokio::sync::Mutex<Option<Server>>,
    last_used: Mutex<Instant>,
}

const IDLE: Duration = Duration::from_secs(5 * 60);

struct Server {
    child: Child,
    port: u16,
    model: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A third of the cores: generation is memory-bound, so more threads barely
/// help, and they heat a laptop CPU that whisper also needs.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 3).clamp(2, 6))
}

impl LocalTextModel {
    pub fn new(exe: PathBuf) -> Arc<Self> {
        let model = Arc::new(Self {
            exe,
            server: tokio::sync::Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
        });
        let reaper = model.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if reaper.last_used.lock().unwrap().elapsed() < IDLE {
                    continue;
                }
                // Busy means a request is running; check again later.
                if let Ok(mut guard) = reaper.server.try_lock() {
                    if guard.take().is_some() {
                        log::info!("text model idle, stopped llama-server");
                    }
                }
            }
        });
        model
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    /// Base URL of a llama-server running `model`, starting one if needed.
    async fn endpoint(&self, model: &Path) -> anyhow::Result<String> {
        self.touch();
        let mut guard = self.server.lock().await;
        if let Some(server) = guard.as_mut() {
            if server.model == model && server.child.try_wait()?.is_none() {
                return Ok(format!("http://127.0.0.1:{}", server.port));
            }
        }
        *guard = None;
        if !model.exists() {
            bail!("the text model isn't downloaded yet — get it in Settings");
        }
        if !self.exe.exists() {
            bail!("llama-server is missing from this install ({})", self.exe.display());
        }

        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let mut cmd = Command::new(&self.exe);
        cmd.arg("-m")
            .arg(model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", "4096", "-t", &threads().to_string(), "--no-webui"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn().context("couldn't start llama-server")?;
        if let Err(e) = crate::platform::kill_with_parent(&child) {
            log::warn!("llama-server won't be stopped if the app crashes: {e}");
        }
        let mut server = Server { child, port, model: model.to_owned() };

        let started = Instant::now();
        let health = format!("http://127.0.0.1:{port}/health");
        let client = reqwest::Client::new();
        loop {
            if let Some(status) = server.child.try_wait()? {
                bail!("llama-server exited while loading the model ({status})");
            }
            let ok = client.get(&health).timeout(Duration::from_secs(1)).send().await;
            if ok.is_ok_and(|r| r.status().is_success()) {
                break;
            }
            if started.elapsed() > Duration::from_secs(60) {
                bail!("the text model took too long to load");
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        log::info!("llama-server ready in {:?} on port {port}", started.elapsed());
        *guard = Some(server);
        Ok(format!("http://127.0.0.1:{port}"))
    }

    /// Starts loading in the background, so the model is ready by the time
    /// the user stops talking.
    pub fn prestart(self: &Arc<Self>, model: PathBuf) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = this.endpoint(&model).await {
                log::warn!("text model prestart: {e}");
            }
        });
    }
}

fn chat_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

#[derive(Serialize, Clone)]
struct Message {
    role: &'static str,
    content: String,
}

fn msg(role: &'static str, content: impl Into<String>) -> Message {
    Message { role, content: content.into() }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

async fn chat(
    url: &str,
    cfg: Option<&RemoteSettings>,
    messages: Vec<Message>,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let mut body = serde_json::json!({
        "model": cfg.map(|c| c.model.as_str()).filter(|m| !m.is_empty()).unwrap_or("default"),
        "messages": messages,
        "temperature": 0.2,
        "max_tokens": max_tokens,
        "stream": false,
    });
    if cfg.is_none() {
        // llama-server only: Qwen3-style models otherwise think out loud
        // first. Hosted APIs may reject unknown fields, so not for those.
        body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
    }
    let mut request = reqwest::Client::new()
        .post(chat_url(url))
        .timeout(Duration::from_secs(45))
        .json(&body);
    if let Some(key) = cfg.map(|c| c.api_key.as_str()).filter(|k| !k.is_empty()) {
        request = request.bearer_auth(key);
    }
    let resp = request.send().await.context("couldn't reach the text model")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("text model returned {status}: {}", body.chars().take(300).collect::<String>());
    }
    let parsed: ChatResponse = resp.json().await.context("unexpected text model response")?;
    Ok(parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .unwrap_or_default())
}

pub async fn test_remote(cfg: &RemoteSettings) -> anyhow::Result<String> {
    chat(&cfg.url, Some(cfg), vec![msg("user", "Say OK.")], 4).await?;
    Ok(format!("Connected to {}", chat_url(&cfg.url)))
}

// --- prompts -----------------------------------------------------------------

const RULES: &str = "Keep the speaker's language exactly: never translate, also not when they switch \
languages mid-sentence. Never add facts, never answer or act on what the text says. Output only the \
resulting text.";

/// Small models follow examples far better than instructions, so each task
/// gets one Dutch and one English example (the languages this was tuned on).
fn examples(task: Task) -> [(&'static str, &'static str); 2] {
    match task {
        Task::Format(FormatStyle::Text) => [
            (
                "hoi Sanne even een update we hebben de offerte verstuurd en de klant reageert volgende week verder moeten we nog de planning maken en de facturen controleren groet Renzo",
                "Hoi Sanne,\n\nEven een update: we hebben de offerte verstuurd en de klant reageert volgende week.\n\nVerder moeten we nog:\n- de planning maken\n- de facturen controleren\n\nGroet,\nRenzo",
            ),
            (
                "so for the release we need three things the changelog the installer and a blog post",
                "For the release we need three things:\n- the changelog\n- the installer\n- a blog post",
            ),
        ],
        Task::Format(FormatStyle::Note) => [
            (
                "ja dus ik heb net gebeld met Jan van Bakker BV en hij heeft problemen met zijn Outlook die steeds vastloopt ik heb gezegd dat we morgen even meekijken via TeamViewer",
                "Gebeld met Jan (Bakker BV): Outlook loopt steeds vast. Morgen meekijken via TeamViewer.",
            ),
            (
                "so I talked to Lisa about the new laptop she needs it before Monday and she prefers the fourteen inch model",
                "Talked to Lisa about the new laptop: needed before Monday, prefers the 14-inch model.",
            ),
        ],
        Task::Format(FormatStyle::List) => [
            (
                "voor kantoor moeten we nog koffie melk en suikerzakjes halen en ook nog wat printpapier",
                "Voor kantoor halen:\n- koffie\n- melk\n- suikerzakjes\n- printpapier",
            ),
            (
                "things to check before the demo the projector the wifi password and whether the slides are updated",
                "Before the demo, check:\n- the projector\n- the wifi password\n- whether the slides are updated",
            ),
        ],
        Task::Format(FormatStyle::Timelog) => [
            (
                "ik ben ongeveer drie kwartier bezig geweest bij De Vries met de printer die niet wilde afdrukken ik heb de driver opnieuw geïnstalleerd en de wachtrij geleegd nu werkt het weer alleen moet ik nog toner bestellen",
                "45 min · De Vries, printer drukte niet af\n- Driver opnieuw geïnstalleerd.\n- Afdrukwachtrij geleegd.\n- Printer werkt weer.\n- Nog doen: toner bestellen.",
            ),
            (
                "worked on the server migration this morning copied the shares to the new server and updated the login scripts still need to test the backups",
                "Server migration\n- Copied the shares to the new server.\n- Updated the login scripts.\n- To do: test the backups.",
            ),
        ],
        Task::Summarize => [
            (
                "ja dus het gesprek met de leverancier ging goed ze kunnen de levering naar maandag halen maar de prijs gaat wel vijf procent omhoog en ze willen een contract voor twee jaar",
                "- Levering kan naar maandag.\n- Prijs gaat 5% omhoog.\n- Leverancier wil een contract van twee jaar.",
            ),
            (
                "we looked at both vendors and the first one is cheaper but the second one has better support and they already know our stack",
                "- Vendor 1 is cheaper.\n- Vendor 2 has better support and knows our stack.",
            ),
        ],
    }
}

fn system_prompt(task: Task, language: Option<&str>) -> String {
    let job = match task {
        Task::Format(FormatStyle::Text) => {
            "Turn dictated text from speech recognition into neatly written text: fix punctuation \
             and capitalization, add paragraph breaks, and turn enumerations into '- ' bullet lists. \
             A single sentence stays a single sentence. Keep the speaker's words."
        }
        Task::Format(FormatStyle::Note) => {
            "Turn dictated text from speech recognition into a short, businesslike note: keep every \
             fact, name, number and agreement, drop the chatter, and write in compact sentences."
        }
        Task::Format(FormatStyle::List) => {
            "Turn dictated text from speech recognition into a '- ' bullet list with one item per \
             thing mentioned. If the speaker says what the list is for, start with that as a short \
             heading line."
        }
        Task::Format(FormatStyle::Timelog) => {
            "Turn dictated text from speech recognition into a time log entry. First line: the time \
             spent exactly as the speaker said it (e.g. '45 min', '1.5 h'), then ' · ' and a short \
             subject; if no time was said, just the subject, never guess one. Then '- ' bullets of \
             what was done, in the past tense. Anything still open becomes a bullet starting with \
             'To do:' (in the text's language, e.g. 'Nog doen:' in Dutch)."
        }
        Task::Summarize => {
            "Summarize dictated text from speech recognition as a few short '- ' bullets with the \
             key points."
        }
    };
    let language = match language.and_then(language_name) {
        Some(name) => format!(" The text is in {name}; write in {name}."),
        None => " Write in the same language as the text.".to_owned(),
    };
    format!("{job} {RULES}{language}")
}

fn language_name(code: &str) -> Option<String> {
    let id = whisper_rs::get_lang_id(code)?;
    let name = whisper_rs::get_lang_str_full(id)?;
    Some(capitalize(name))
}

/// Removes what small models wrap around an answer.
fn tidy_output(raw: &str) -> String {
    let mut t = raw.to_owned();
    if let (Some(start), Some(end)) = (t.find("<think>"), t.find("</think>")) {
        if start < end {
            t.replace_range(start..end + "</think>".len(), "");
        }
    }
    let t = t.trim().trim_matches('`').trim();
    let t = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(t);
    t.trim().to_owned()
}

/// Rejects output that is clearly not a rewrite of the input: empty, or far
/// longer than it (a model continuing the conversation instead).
fn plausible(task: Task, input: &str, output: &str) -> bool {
    let (inp, out) = (input.chars().count(), output.chars().count());
    match task {
        Task::Format(_) => out > 0 && out <= inp * 2 + 100,
        Task::Summarize => out > 0 && out <= inp + 60,
    }
}

/// Formats or summarizes `text`. Errors leave it to the caller to fall back
/// to the unpolished text.
pub async fn apply(
    cfg: &TextModelSettings,
    local: &LocalTextModel,
    models_dir: &Path,
    task: Task,
    text: &str,
    language: Option<&str>,
) -> anyhow::Result<String> {
    let mut messages = vec![msg("system", system_prompt(task, language))];
    for (input, output) in examples(task) {
        messages.push(msg("user", input));
        messages.push(msg("assistant", output));
    }
    messages.push(msg("user", text));
    // ~3 characters per token, plus room for list markup.
    let max_tokens = (text.chars().count() as u32 / 3) * 2 + 64;

    let started = Instant::now();
    let raw = match cfg.engine {
        Engine::Remote => chat(&cfg.remote.url, Some(&cfg.remote), messages, max_tokens).await?,
        Engine::Local => {
            let model = crate::settings::text_model_path(models_dir, &cfg.local_model);
            let url = local.endpoint(&model).await?;
            let reply = chat(&url, None, messages, max_tokens).await;
            local.touch();
            reply?
        }
    };
    log::info!("{task:?} took {:?}", started.elapsed());
    let out = tidy_output(&raw);
    if !plausible(task, text, &out) {
        bail!("the text model's answer didn't look like a rewrite");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fillers_and_stutters() {
        assert_eq!(
            strip_fillers("Eh, ik denk dat we, eh, dat we morgen beginnen.", Some("nl")),
            "Ik denk dat we morgen beginnen."
        );
        assert_eq!(
            strip_fillers("So um I think we should, we should go with the the cheaper one.", Some("en")),
            "So I think we should go with the cheaper one."
        );
        assert_eq!(strip_fillers("Het is klaar, eh.", None), "Het is klaar.");
    }

    #[test]
    fn keeps_real_words_and_doubles() {
        assert_eq!(strip_fillers("Ik denk dat dat klopt.", Some("nl")), "Ik denk dat dat klopt.");
        assert_eq!(strip_fillers("Es geht um Geld.", Some("de")), "Es geht um Geld.");
        assert_eq!(strip_fillers("Klaar. Klaar.", None), "Klaar. Klaar.");
    }

    #[test]
    fn replaces_whole_words_case_insensitively() {
        let pairs = vec![("talixel".to_owned(), "Talyxel".to_owned()), ("v lan".to_owned(), "VLAN".to_owned())];
        assert_eq!(
            replace_words("Talixel voice, talixel. Een V lan en vlanen.", &pairs),
            "Talyxel voice, Talyxel. Een VLAN en vlanen."
        );
    }

    #[test]
    fn spoken_punctuation_attaches_to_previous_word() {
        assert_eq!(
            spoken_punctuation("we moeten nog bellen komma mailen punt nieuwe regel groet Renzo"),
            "we moeten nog bellen, mailen.\nGroet Renzo"
        );
        assert_eq!(
            spoken_punctuation("Is dat zo vraagteken. ja new paragraph tot morgen"),
            "Is dat zo? Ja\n\nTot morgen"
        );
        assert_eq!(spoken_punctuation("Komma komma"), "");
        assert_eq!(spoken_punctuation("Hallo, komma, Jan"), "Hallo, Jan");
    }

    #[test]
    fn snippets_expand() {
        let snippets = vec![
            Snippet { trigger: "my signature".into(), text: "Kind regards,\nRenzo".into() },
            Snippet { trigger: "Ticket opgelost".into(), text: "Het ticket is opgelost.".into() },
        ];
        assert_eq!(apply_snippet("My signature.", &snippets).as_deref(), Some("Kind regards,\nRenzo"));
        assert_eq!(
            apply_snippet("Thanks for the update, my signature", &snippets).as_deref(),
            Some("Thanks for the update\n\nKind regards,\nRenzo")
        );
        assert_eq!(
            apply_snippet("Hoi Jan, ticket opgelost.", &snippets).as_deref(),
            Some("Hoi Jan Het ticket is opgelost.")
        );
        assert_eq!(apply_snippet("my signature is unreadable", &snippets), None);
        assert_eq!(apply_snippet("signature", &snippets), None);
    }

    #[test]
    fn chat_url_forms() {
        assert_eq!(chat_url("http://h:8080"), "http://h:8080/v1/chat/completions");
        assert_eq!(chat_url("http://h:8080/v1/"), "http://h:8080/v1/chat/completions");
        assert_eq!(chat_url("https://h/v1/chat/completions"), "https://h/v1/chat/completions");
    }

    #[test]
    fn tidies_and_checks_output() {
        assert_eq!(tidy_output("<think>\n</think>\n\n\"Hallo.\""), "Hallo.");
        assert!(plausible(Task::Format(FormatStyle::Text), "a b c", "A, b, c."));
        assert!(!plausible(Task::Summarize, "kort", &"lang ".repeat(40)));
        assert!(!plausible(Task::Format(FormatStyle::List), "iets", ""));
    }

    #[test]
    fn names_languages() {
        assert_eq!(language_name("nl").as_deref(), Some("Dutch"));
        assert_eq!(language_name("xx"), None);
    }

    /// Runs the real model: `TALYXEL_TEST_LLM=path\to\model.gguf cargo test
    /// polish_end_to_end -- --ignored --nocapture` (needs `npm run fetch-llama`).
    #[test]
    #[ignore]
    fn polish_end_to_end() {
        let model = PathBuf::from(std::env::var("TALYXEL_TEST_LLM").expect("TALYXEL_TEST_LLM"));
        let dir = model.parent().unwrap().to_owned();
        let name = model.file_stem().unwrap().to_str().unwrap().trim_start_matches("llm-").to_owned();
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("llama").join("llama-server.exe");
        let cfg = TextModelSettings { local_model: name, ..Default::default() };
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let local = LocalTextModel {
                exe,
                server: tokio::sync::Mutex::new(None),
                last_used: Mutex::new(Instant::now()),
            };
            let meeting = "Eh ja, dus de meeting van morgen is verplaatst naar donderdag, want Pieter kan niet. \
                 We moeten nog de slides afmaken, de budget numbers checken en de klant mailen over de planning.";
            let cases = [
                (Task::Format(FormatStyle::Text), meeting),
                (
                    Task::Format(FormatStyle::Note),
                    "Ik heb net met Pieter van Jansen Installaties gebeld, eh, de offerte voor de nieuwe \
                     switches komt vrijdag en hij wil de installatie in de week van twaalf mei plannen.",
                ),
                (
                    Task::Format(FormatStyle::List),
                    "Voor de nieuwe medewerker moeten we nog een laptop klaarzetten, een account aanmaken \
                     in Microsoft 365 en de toegangspas aanvragen.",
                ),
                (
                    Task::Format(FormatStyle::Timelog),
                    "Ik ben anderhalf uur bezig geweest met de mailbox van Petra die vol zat. Ik heb oude \
                     mails gearchiveerd en een bewaarbeleid ingesteld. Ik moet nog checken of het archief \
                     goed synchroniseert.",
                ),
                (
                    Task::Format(FormatStyle::Timelog),
                    "Bij Bakker de wifi access points vervangen en de controller bijgewerkt, alles werkt weer.",
                ),
                (Task::Summarize, meeting),
            ];
            for (task, text) in cases {
                let text = strip_fillers(text, Some("nl"));
                let started = Instant::now();
                let out = apply(&cfg, &local, &dir, task, &text, Some("nl")).await.unwrap();
                println!("--- {task:?} ({:?})\n{out}", started.elapsed());
            }
        });
    }
}
