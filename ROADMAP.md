# TalyxelVoice roadmap

Goal: close the gap with Wispr Flow for daily dictation on Windows, **without giving up
local-first**. Every feature below runs with no cloud account; "your own server" stays the only
network path. Features that need the active window's text or a cloud service are deferred or out.

Phases are ordered by value per unit of work. Sizes: S = a day, M = a few days, L = a week or more.
No dates: ship a phase when it is solid, then release.

## Where we are (v0.1.3)

| Wispr Flow feature | TalyxelVoice today |
|---|---|
| Dictate into any text field, push-to-talk, hands-free | Yes (`pipeline.rs`, `platform.rs`) |
| 100+ languages, auto-detect, multilingual | Yes, via whisper; `spoken_languages` steers code-switching |
| Auto Cleanup (None / Light / Medium) | `Off` / `Clean` (rules) / `Format` (text model) |
| Dictionary | Yes, as a whisper prompt hint |
| Dictation history | In memory only, last 50, lost on restart |
| Microphone choice, autostart, notifications | Mic yes; autostart no |
| Undo AI edit, snippets, Transforms, Command Mode | No |
| Context awareness, Styles, Voice Profile | No |
| Scratchpad, Notetaker, MCP, teams | No |

## Phase 1: trust the output (v0.2)

The polish step changes words. Users need to see and reverse that.

1. **Keep the raw transcript** (S). `HistoryEntry` gets `raw: String` next to `text`, plus which
   polish mode produced it. The pipeline already has both strings at paste time.
2. **Undo AI edit** (S). Hotkey (default `Ctrl+Shift+Z`-style, configurable) and a button in the
   history list that pastes the raw text of the last dictation. Reuses `insert_text`.
3. **Re-paste last dictation** (S). Same mechanism, pastes `text` again. Covers "the field lost
   focus" and "I pasted into the wrong window".
4. **Persist history** (M). JSON lines file next to `settings.json`, capped by count and age,
   with a "keep history" toggle and a Clear button. Search box in the Dictation tab.
   Default retention: 14 days. No audio is stored.
5. **Latency and word counts** (S). History already records latency; show a per-day word count and
   average latency per engine so users can compare local vs server.

Done when: a wrong rewrite can be reverted in one keystroke, and yesterday's dictation is findable.

## Phase 2: say less, get more (v0.3)

Rule-based features that need no model and work in every language.

1. **Snippets** (M). Settings tab with trigger phrase → text. After `strip_fillers`, if the whole
   utterance matches a trigger (case- and punctuation-insensitive), paste the snippet instead.
   Partial match ("my signature" at the end of a sentence) is a stretch goal. Plain text only;
   rich text depends on the target app and is out of scope.
2. **Spoken punctuation and newlines** (S). "new line", "new paragraph", "comma", "question mark"
   in the user's languages, applied in `polish.rs` before the model step. Off by default when
   `Polish::Off`.
3. **Dictionary improvements** (M). Star words to put them first in the whisper prompt (the prompt
   is length-limited, so order matters). Optional replacement pairs (`heard → want`) applied after
   transcription, for names whisper keeps misspelling even with the hint.
4. **Autostart at login and start minimized** (S). Tauri autostart plugin; tray already exists.

Done when: a support engineer can dictate a ticket reply with their standard closing without
touching the keyboard.

## Phase 3: a better rewrite (v0.4)

Everything that depends on the text model. The bundled qwen2.5-1.5b is the floor; these must
degrade gracefully (paste the plain text, say so in the pill) exactly as `Format` does now.

1. **Self-corrections** (M). Prompt and examples for "no wait, Friday at three" → keep only the
   correction. Add to `Polish::Clean` as an opt-in "Medium" level so the rules stay the default.
   Measure on a fixed set of Dutch and English clips before enabling; the small model will fail
   on some of these, and the plausibility check in `polish.rs` must catch rewrites that drop
   content.
2. **List and number detection** (S). Covered by the existing `Format` prompt; add examples for
   "one, ... two, ..." spoken enumerations and dates/times.
3. **Model choice in the UI** (M). Let users download a 3B or 7B text model when they have the
   RAM, and show the expected latency. Server users already get this for free.
4. **Per-app default mode** (M). `FocusedApp` is already captured at hotkey time. A small table
   "process name → polish mode / format style" lets chat apps get `Clean` and mail apps `Format`.
   This is the local-first stand-in for Wispr's context awareness: it reads the window title and
   process, never the field's contents.

Done when: Medium cleanup is on by default for new installs and the raw/undo path is used rarely.

## Phase 4: act on existing text (v0.5)

Wispr's Transforms and Command Mode. These need the user's selection, which means a
Ctrl+C round trip through the clipboard, and a model good enough to follow an instruction.

1. **Transform selection** (L). Hotkey copies the selection (same `SendInput` path as paste),
   runs a preset (Polish, Make a list, Translate to English, Shorter), pastes the result over the
   selection, and keeps the original in history for undo. Word limit and a timeout; the pill shows
   progress because a 7B model on CPU takes long.
2. **Spoken command on selection** (L). The summary hotkey already shows how a second hotkey can
   change the task. A third hotkey records an instruction, then applies it to the selection.
   Requires at least the 3B model; with the bundled 1.5B this is off and the UI says why.
3. **Diff view** (M). Before/after in the settings window for the last transform.

Done when: "make this more formal" on a selected paragraph works in Outlook and VS Code.

## Deferred or out of scope

- **Reading the active text field** for context awareness. Needs UI Automation and sends field
  contents to the model; conflicts with the privacy promise. Per-app defaults (phase 3) are the
  substitute.
- **Styles and Voice Profile**. Learning the user's writing from their dictations is a data
  retention feature first; revisit after persisted history has been in use for a while.
- **Scratchpad, Notetaker, meeting transcription, speaker identification.** Different product.
  Long-form transcription of a meeting is a separate mode with a different audio path and UI.
- **MCP, teams, shared dictionaries, SSO.** No accounts in this app. A shared dictionary could be
  a file on a network share if a team asks for it.
- **macOS and Linux.** `platform.rs` has stubs; a port is its own project.

## Release discipline

- Each phase ends with a release (`npm run release -- x.y.0`), a README update, and the
  `cargo test` suite plus the opt-in end-to-end test on the fixed clip set.
- New model-dependent behavior ships opt-in first, becomes default one release later.
- Any feature that stores more data (history, snippets) gets a visible toggle and a Clear button in
  the same release.
