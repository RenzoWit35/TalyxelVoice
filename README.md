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
| Cleanup & summary | `src-tauri/src/polish.rs` | Rule-based filler/stutter removal, then optional formatting or a summary by a small text model (bundled `llama-server`, or your own chat endpoint). |
| Engines | `src-tauri/src/stt.rs` | `LocalWhisper` keeps the model + decoder state warm; the remote engine speaks the OpenAI transcription API. |
| OS glue | `src-tauri/src/platform.rs` | Focused-app detection, paste via `SendInput`, overlay that never steals focus. |
| History | `src-tauri/src/history.rs` | Raw and pasted text per dictation, in `history.json`, pruned by age. |
| Settings UI | `index.html`, `src/main.ts` | Engine, models, server, language and dictionary, snippets, per-app modes, hotkeys, microphone. |
| Overlay pill | `pill.html`, `src/pill.ts` | Live level meter / "Transcribing" / errors, bottom-center above the taskbar. |

Settings: `%APPDATA%\com.talyxel.voice\settings.json` · Models: `%APPDATA%\com.talyxel.voice\models\`

## Run it

Prerequisites on Windows: Rust, Node 22+, **VS Build Tools** (C++), **LLVM** (`winget install LLVM.LLVM`,
for whisper's bindgen), the **Vulkan SDK** (`winget install KhronosGroup.VulkanSDK`, for whisper's GPU
backend; it sets `VULKAN_SDK`), and `ninja.exe` on PATH (VS Build Tools ships one under
`Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja`). `src-tauri/.cargo/config.toml` explains the
build settings; the short version is that without Ninja the `cmake` crate builds whisper.cpp
**unoptimized** under MSVC, which makes it ~50x slower. Whisper's Vulkan shader generator builds in
deeply nested folders, so keep `target/` under ~40 characters of path (or set `CARGO_TARGET_DIR`
to something like `C:\t`); past MSVC's 260-character limit it fails with "Cannot open compiler
generated file".

```bash
npm install
npm run tauri dev
```

Then open the Engine tab and download a model (`small-q5_1` is a good default on CPU).

`npm run tauri dev`/`build` first runs `npm run fetch-llama`, which downloads a pinned llama.cpp Vulkan
build into `src-tauri/llama/` and the Vulkan loader into `src-tauri/vulkan/` (both checksum-verified).
They are bundled as resources; plain `cargo` commands need them to exist too, so run
`npm run fetch-llama` once after cloning.

### GPU

Whisper and the text model run on the graphics card through Vulkan, so NVIDIA, AMD and Intel all
work, with the CPU as the fallback (Engine → "Run on the graphics card"). With several GPUs,
`src-tauri/src/gpu.rs` picks the dedicated one with the most memory over the CPU's own; text-model
layers that don't fit in its memory stay on the CPU. Only GPU drivers install `vulkan-1.dll`, so the
app delay-loads it and falls back to the bundled loader, which finds no devices, on a PC without one.

## Cleanup, formatting and summaries

After whisper, each dictation goes through the mode picked under **Cleanup & summary**:

- **As spoken**: pasted as heard.
- **Clean up** (default): filler words (uh, um, eh) and stutters ("we should, we should") are
  removed by rules. Instant, no model.
- **Medium**: clean up, then a text model fixes punctuation and self-corrections only.
- **Format**: clean up, then a text model adds paragraphs and bullet lists.

The **summary hotkey** (default `Alt+Shift+Space`) works like the dictation hotkey but pastes a
few bullets summarizing what you said.

The default text model is `qwen2.5-1.5b` (1 GB), picked because smaller models translated Dutch to
English; `qwen2.5-3b` and `qwen2.5-7b` can be downloaded in the same place and do noticeably better
at Medium cleanup, transforms and commands, at the cost of RAM and seconds.
It runs in a separate `llama-server` process: started when you press a hotkey that needs it (so it
loads while you talk), stopped after 5 minutes idle, and tied to the app with a Windows job object so
it can't outlive a crash. It uses a third of the cores, and adds a few seconds per dictation on a
laptop CPU. For better and faster results, point it at a bigger model on your server
(`/v1/chat/completions`: llama-server, Ollama, LM Studio, vLLM). If the model fails or answers with
something that isn't a rewrite, the plain text is pasted and the pill says so.

## Hotkeys

| Default | Does |
|---|---|
| `Ctrl+Shift+Space` | Hold to dictate, tap for hands-free. Pastes where the cursor is. |
| `Alt+Shift+Space` | Same, but pastes a few summary bullets. |
| `Ctrl+Alt+P` | Rewrites the **selected** text with the chosen transform: polish, list, translate to English, shorter, friendlier, formal, or prompt engineer (rough thoughts into a structured AI prompt). |
| `Ctrl+Alt+Space` | Hold, say what to do with the selected text ("make this more formal"), release. |
| `Ctrl+Alt+Z` | Undo the cleanup: pastes the last dictation (or the text a transform replaced) as it was. |
| `Ctrl+Alt+V` | Pastes the last dictation again. |

All of them are configurable under General and Cleanup & summary; Backspace in a hotkey field turns
it off. Transforms and commands copy the selection with a simulated `Ctrl+C`, so they work in any
app that supports it, up to 3000 words. With the bundled 1.5B model, Polish and List work; commands
tend to come back in English and Medium misses some self-corrections, so for those pick the 3B
model or your own server.

## Cleanup levels

| Level | What happens | Model |
|---|---|---|
| As spoken | Pasted as heard. | none |
| Clean up (default) | Fillers (uh, um, eh) and stutters ("we should, we should") removed by rules. | none |
| Medium | Clean up, then punctuation fixed and self-corrections resolved ("Thursday, no wait, Friday" keeps Friday). Words stay yours. | text model |
| Format | Clean up, then reshaped as text, note, list, time log, or a structured AI prompt. | text model |

**Per application**: a table under Cleanup & summary maps a program's file name (`OUTLOOK.EXE`,
`Code.exe`, as shown in the history) to its own level, so mail gets Format and a terminal gets As
spoken. This is the local-first stand-in for "context awareness": it reads which window is focused,
never what's in it.

## Words, punctuation and snippets

- **Dictionary** (Language & words): one word per line goes into whisper's prompt. `*Entra ID` puts
  it first (the prompt is cut after a few hundred tokens). `share point => SharePoint` replaces what
  whisper keeps writing, whole words, any case, before any cleanup.
- **Spoken punctuation** (off by default): "comma", "period", "question mark", "new line",
  "new paragraph" and their Dutch, German and French names become the punctuation they name.
- **Snippets**: say a trigger phrase on its own ("my signature") or at the end of a sentence
  ("thanks for the update, my signature") and its text goes in instead, skipping cleanup and
  formatting.

## Notes

The Notes tab is a place to dictate into that isn't someone else's text field. **Dictate here** on a
note routes every dictation (and summary) into that note instead of pasting it, until you turn it
off; **To note** on a history entry adds that dictation afterwards. Notes can be pinned, searched,
copied, and rewritten in place with any transform (polish, list, shorter, friendlier, formal,
translate, prompt engineer) with one-step undo. They live in `notes.json` next to the settings.

## History

The Dictation tab lists what you said and what went in, with a word diff behind **Changes**. It is
kept in `history.json` next to the settings (text only, never audio), pruned after 14 days by
default; both are under General, next to a Clear button and **Start with Windows**.

## Tray

The tray icon's menu opens the settings, checks for updates, and has a **Language** submenu to
switch whisper's language for the next dictation without opening the window.

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
`TALYXEL_TEST_MODEL=… TALYXEL_TEST_WAV=… cargo test --release -- --ignored` for whisper, and
`TALYXEL_TEST_LLM=…\llm-qwen2.5-1.5b.gguf cargo test --release polish_end_to_end -- --ignored --nocapture`
for every text-model task (cleanup, formats, summary, transforms, a command).

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

## Roadmap

See [ROADMAP.md](ROADMAP.md). Shipped so far: core dictation, cleanup levels and summaries, undo
and history, snippets and spoken punctuation, per-app modes, transforms and commands on selected
text, Vulkan GPU acceleration. Still open: a modifier-only hotkey (hold Right-Ctrl) via a low-level
keyboard hook, streaming partial results, macOS.

## Third-party

`llama-server` and its DLLs come unmodified from [llama.cpp](https://github.com/ggml-org/llama.cpp)
(MIT), build pinned in `scripts/fetch-llama.mjs`. `vulkan/vulkan-1.dll` is the unmodified
[Vulkan loader](https://github.com/KhronosGroup/Vulkan-Loader) (Apache-2.0) from LunarG's runtime. The default text model is
[Qwen2.5-1.5B-Instruct](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF) (Apache-2.0),
downloaded on request.
