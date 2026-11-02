# TalyxelVoice

Voice dictation for every text field on your computer, in the spirit of Wispr Flow,
but **local-first**: speech is transcribed on your machine by whisper.cpp, or by
**your own server** running a bigger model. Audio never goes to a third party
unless you point it there.

Hold the hotkey, talk, let go: the text is pasted wherever your cursor is.

## How it works

```
 hotkey (global)        audio (cpal)          speech-to-text               insert
 ───────────────►  record mic at native  ──►  local: whisper.cpp      ──►  clipboard + Ctrl+V,
 hold = talk        rate, mono, 16 kHz        (whisper-rs, in-process)     then restore the
 tap  = hands-free  resample on stop          remote: POST /v1/audio/      previous clipboard
                                              transcriptions (your server)
```

| Piece | File | Notes |
|---|---|---|
| Hotkey state machine | `src-tauri/src/pipeline.rs` | Hold to talk; a tap (<350 ms) toggles hands-free, tap again to stop. |
| Audio capture | `src-tauri/src/audio.rs` | Stream runs on its own thread only while recording, so the Windows mic indicator is off when idle. |
| Engines | `src-tauri/src/stt.rs` | `LocalWhisper` keeps the model + decoder state warm; the remote engine speaks the OpenAI transcription API. |
| OS glue | `src-tauri/src/platform.rs` | Focused-app detection, paste via `SendInput`, overlay that never steals focus. |
| Settings UI | `index.html`, `src/main.ts` | Engine, models, server, language and dictionary, hotkey, microphone. |
| Overlay pill | `pill.html`, `src/pill.ts` | Live level meter / "Transcribing" / errors, bottom-center above the taskbar. |

Settings: `%APPDATA%\com.talyxel.voice\settings.json` · Models: `%APPDATA%\com.talyxel.voice\models\`

## Run it

Prerequisites on Windows: Rust, Node 22+, **VS Build Tools** (C++), **LLVM** (`winget install LLVM.LLVM`,
for whisper's bindgen), and `ninja.exe` on PATH (VS Build Tools ships one under
`Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja`). `src-tauri/.cargo/config.toml` explains the
build settings; the short version is that without Ninja the `cmake` crate builds whisper.cpp
**unoptimized** under MSVC, which makes it ~50x slower.

```bash
npm install
npm run tauri dev
```

Then open the Engine tab and download a model (`small-q5_1` is a good default on CPU).

## Using your own server

Pick **My server** in the Engine tab. Anything that implements OpenAI's
`POST /v1/audio/transcriptions` works, for example:

```bash
# speaches (faster-whisper), GPU box
docker run --gpus all -p 8000:8000 ghcr.io/speaches-ai/speaches:latest-cuda
# → URL http://<server>:8000, model Systran/faster-whisper-large-v3
```

```bash
# whisper.cpp's own server
whisper-server -m ggml-large-v3.bin --host 0.0.0.0 --port 8080 --inference-path /v1/audio/transcriptions
```

The URL can be the base (`http://host:8000`), `.../v1`, or the full endpoint. An API key is optional
and sent as a Bearer token.

## Speed (local, Intel Core Ultra 5 125U, CPU only, 7 s clip)

| Model | Language set | Auto-detect |
|---|---|---|
| small-q5_1 | 2.3 s | 4.8 s |

Auto-detect runs the encoder twice; set your language when you can. `base` is ~3x faster with
lower accuracy. The encoder is sized to the clip (`audio_ctx`) rather than whisper's fixed 30 s window.

Tests: `cargo test` (unit), plus opt-in end-to-end ones:
`TALYXEL_TEST_MODEL=… TALYXEL_TEST_WAV=… cargo test --release -- --ignored`.

## Updates and releases

The app checks
`https://github.com/RenzoWit35/TalyxelVoice/releases/latest/download/latest.json` 15 s after start
and every 6 hours (`src-tauri/src/updates.rs`). A newer version downloads in the background and is
verified against the public key in `tauri.conf.json` (the signature also binds the version number).
Then the tray and Settings offer **Restart & update**. It never installs during a dictation.

To ship a version:

```bash
npm run release -- 0.2.0
```

```bash
git push --follow-tags
```

The `Release` workflow builds on Windows, signs, and publishes the installer plus `latest.json`.
One-time setup:

- The repo must be **public**, or the updater can't download release assets.
- Repo secrets: `TAURI_SIGNING_PRIVATE_KEY` (contents of `~/.tauri/talyxel-voice.key`) and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (empty).

**Back up the private key.** If it's lost, installed copies can never be updated again. You'd have
to ship a new public key by hand.

Local signed build:
`TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/talyxel-voice.key)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" npx tauri build`.
To test the whole flow without GitHub, a debug build honours `TALYXEL_UPDATE_URL` (run
`tauri dev --config '{"plugins":{"updater":{"dangerousInsecureTransportProtocol":true}}}'` for an
http feed).

## Roadmap: the Talyxel twist

- [x] **M1 — Core dictation.** Push-to-talk and hands-free, local whisper or own server, paste
      anywhere, clipboard restore, history, personal dictionary, language and code-switching hints.
- [ ] **M2 — Per-app voice modes.** The focused app is already captured at key-down
      (`FocusedApp.exe`). Map app → mode (Outlook: email tone; VS Code/terminals: code-aware
      casing, no trailing period; Slack: casual; Notion/Obsidian: Markdown), then run an optional
      cleanup pass through a local LLM (LM Studio / Ollama, OpenAI-compatible) or your server.
- [ ] **M3 — Command mode.** A second hotkey: speak an instruction ("make this more formal",
      "translate to Dutch", "turn into bullet points") and it is applied to the selected text
      (copy selection → LLM → paste back).
- [ ] **M4 — Better code-switching.** Per-segment language detection with large-v3 on the server,
      learned dictionary (corrections you make get added), filler-word removal and spoken
      punctuation ("new paragraph", "comma").
- [ ] **Later:** Vulkan GPU build for Intel Arc / AMD iGPUs, a modifier-only hotkey (e.g. hold
      Right-Ctrl) via a low-level keyboard hook, streaming partial results, macOS.
