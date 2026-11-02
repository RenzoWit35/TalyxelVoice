# TalyxelVoice roadmap

Goal: close the gap with Wispr Flow for daily dictation on Windows, **without giving up
local-first**. Every feature below runs with no cloud account; "your own server" stays the only
network path. Features that need the active window's text or a cloud service are deferred or out.

Phases are ordered by value per unit of work.

## Status

Phases 1 to 4 below shipped together in **v0.2.0** (November 2026). They are kept here as the
record of what was built and why; the "done when" lines are what to check if something regresses.

| Wispr Flow feature | TalyxelVoice |
|---|---|
| Dictate into any text field, push-to-talk, hands-free | Yes (`pipeline.rs`, `platform.rs`) |
| 100+ languages, auto-detect, multilingual | Yes, via whisper; `spoken_languages` steers code-switching |
| Auto Cleanup (None / Light / Medium) | As spoken / Clean up (rules) / Medium (model) / Format (model) |
| Undo AI edit | Undo hotkey pastes the raw transcript; word diff in the history |
| Dictionary | Yes, with starred words and `heard => wanted` fixes |
| Snippets | Yes, plain text, alone or at the end of a sentence |
| Spoken punctuation | Yes, opt-in, EN/NL/DE/FR |
| Dictation history | `history.json`, 14 days by default, searchable, with word counts |
| Context awareness | Per-application cleanup level, from the focused window's exe only |
| Transforms (Polish, list, translate, shorter) | Transform hotkey on the selection |
| Command Mode | Command hotkey: hold, speak the instruction, applied to the selection |
| Model choice | 1.5B / 3B / 7B local, or any chat server |
| Autostart | Yes |
| Scratchpad | Notes tab: dictate into a note, pin, search, transforms with undo |
| Prompt Engineer, Empathize | Transforms: prompt, friendlier, formal; Prompt also as a Format style |
| Flow Bar language switch | Tray menu → Language |
| Styles, Voice Profile, Notetaker, MCP, teams | No (see below) |

## Phase 1: trust the output (shipped in v0.2.0)

The polish step changes words. Users need to see and reverse that.

1. **Keep the raw transcript**. `HistoryEntry` has `raw` next to `text`, plus the `mode` that
   produced it.
2. **Undo AI edit**. `Ctrl+Alt+Z` pastes the raw text of the last dictation (or the selection a
   transform replaced).
3. **Re-paste last dictation**. `Ctrl+Alt+V`.
4. **Persist history**. `history.json` next to `settings.json`, capped by count and age, with a
   keep/forget toggle, Clear button and search. No audio is stored.
5. **Word counts and latency**. Today's dictations, words and average latency above the list.

Done when: a wrong rewrite can be reverted in one keystroke, and yesterday's dictation is findable.

## Phase 2: say less, get more (shipped in v0.2.0)

Rule-based features that need no model and work in every language.

1. **Snippets**. Trigger phrase → text, matched after filler removal on the whole utterance or
   its tail. Plain text only.
2. **Spoken punctuation and newlines**. Opt-in, applied before the model step.
3. **Dictionary improvements**. `*word` goes first in the whisper prompt; `heard => wanted` pairs
   are applied after transcription on whole words.
4. **Autostart at login**, hidden in the tray.

Done when: a support engineer can dictate a ticket reply with their standard closing without
touching the keyboard.

## Phase 3: a better rewrite (shipped in v0.2.0)

1. **Self-corrections**: the Medium level, a `Cleanup` task with its own prompt, examples and a
   plausibility check that rejects answers that drop most of the input. Opt-in; Clean up stays
   the default.
2. **List and number detection**: the Format prompt now numbers counted-off items.
3. **Model choice in the UI**: 3B and 7B next to the 1.5B, with RAM and speed notes.
4. **Per-app default mode**: exe → level table, matched case-insensitively without `.exe`.

Still to measure: Medium on a fixed set of Dutch and English clips before making it the default.

## Phase 4: act on existing text (shipped in v0.2.0)

1. **Transform selection**: hotkey copies the selection via simulated `Ctrl+C`, runs the preset,
   pastes over it, keeps the original in history. 3000-word cap, model timeout grows with length.
2. **Spoken command on selection**: a third hold-to-talk hotkey records the instruction.
3. **Diff view**: word-level diff in the history behind **Changes**.

Done when: "make this more formal" on a selected paragraph works in Outlook and VS Code.

## Phase 5: notes (shipped in v0.3.0)

1. **Notes tab**: list, editor, pin, search, copy, delete; `notes.json`.
2. **Dictate here**: a capture toggle routes dictations into the open note instead of pasting.
3. **To note** on history entries; transforms on a whole note with one-step undo.
4. **More transforms**: friendlier, formal, prompt engineer (also a Format style).
5. **Tray language switch**.

Not done from Scratchpad: images, tabs, version history (undo covers one step), sync.

## Next

- Measure Medium and the command prompts on a fixed clip set per model size; promote Medium to
  the default for new installs once the 1.5B passes.
- A modifier-only hotkey (hold Right-Ctrl) via a low-level keyboard hook.
- Rich-text snippets where the target app accepts HTML on the clipboard.
- Note version history (last N versions) and export to a .md file.
- Vulkan GPU build for Intel Arc / AMD iGPUs; streaming partial results.

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
