// The Android app's page. Unlike the desktop window it records the microphone
// itself and sends the samples to Rust (`transcribe`), which runs the same
// whisper engines and cleanup as the desktop pipeline.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Engine = "local" | "remote";
type Polish = "off" | "clean" | "medium" | "format";

interface Remote {
  url: string;
  api_key: string;
  model: string;
}

// Only the fields this page edits; the rest round-trip untouched.
interface Settings {
  engine: Engine;
  local_model: string;
  remote: Remote;
  text_model: { engine: Engine; local_model: string; remote: Remote };
  polish: Polish;
  language: string;
  dictionary: string[];
  spoken_punctuation: boolean;
  keep_history: boolean;
  [key: string]: unknown;
}

interface ModelInfo {
  name: string;
  size_mb: number;
  downloaded: boolean;
}

interface Note {
  id: number;
  title: string;
  body: string;
  pinned: boolean;
  created: number;
  updated: number;
}

interface HistoryEntry {
  text: string;
  raw: string;
  mode: string;
  app: string;
  engine: string;
  latency_ms: number;
  at: number;
}

interface Dictated {
  text: string;
  raw: string;
  mode: string;
  warning: string | null;
  latency_ms: number;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const input = (id: string) => $<HTMLInputElement>(id);

function toast(message: string, error = false) {
  const el = $("toast");
  el.textContent = message;
  el.classList.toggle("error", error);
  el.classList.add("show");
  clearTimeout((el as any)._t);
  (el as any)._t = setTimeout(() => el.classList.remove("show"), error ? 4500 : 1600);
}

async function copyText(text: string) {
  if (!text.trim()) return toast("Nothing to copy", true);
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Older WebViews: the selection-based copy still works on a tap.
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
  toast("Copied");
}

// ---------------------------------------------------------------- tabs

function showTab(name: string) {
  document.querySelectorAll<HTMLElement>(".tab-btn").forEach((b) => b.classList.toggle("active", b.dataset.tab === name));
  document.querySelectorAll<HTMLElement>(".tab").forEach((t) => t.classList.toggle("active", t.id === `tab-${name}`));
  if (name === "history") renderHistory();
  if (name === "notes") renderNotes();
}
document.querySelectorAll<HTMLElement>(".tab-btn").forEach((b) => b.addEventListener("click", () => showTab(b.dataset.tab!)));

// ---------------------------------------------------------------- recording

/** Mono capture through Web Audio, asked for at 16 kHz so whisper needs no resampling. */
class Recorder {
  private ctx?: AudioContext;
  private stream?: MediaStream;
  private node?: ScriptProcessorNode;
  private chunks: Float32Array[] = [];

  async start(onLevel: (level: number) => void) {
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, noiseSuppression: true, autoGainControl: true, echoCancellation: false },
    });
    try {
      this.ctx = new AudioContext({ sampleRate: 16000 });
    } catch {
      this.ctx = new AudioContext();
    }
    if (this.ctx.state === "suspended") await this.ctx.resume();
    const source = this.ctx.createMediaStreamSource(this.stream);
    this.node = this.ctx.createScriptProcessor(4096, 1, 1);
    this.chunks = [];
    this.node.onaudioprocess = (e) => {
      const data = e.inputBuffer.getChannelData(0);
      this.chunks.push(new Float32Array(data));
      let sum = 0;
      for (let i = 0; i < data.length; i++) sum += data[i] * data[i];
      onLevel(Math.sqrt(sum / data.length));
    };
    source.connect(this.node);
    this.node.connect(this.ctx.destination);
  }

  /** 16-bit little-endian PCM and its sample rate. */
  async stop(): Promise<{ pcm: Uint8Array; rate: number }> {
    const rate = this.ctx?.sampleRate ?? 16000;
    this.node?.disconnect();
    this.stream?.getTracks().forEach((t) => t.stop());
    await this.ctx?.close().catch(() => {});
    const length = this.chunks.reduce((n, c) => n + c.length, 0);
    const pcm = new Int16Array(length);
    let offset = 0;
    for (const chunk of this.chunks) {
      for (let i = 0; i < chunk.length; i++) {
        const s = Math.max(-1, Math.min(1, chunk[i]));
        pcm[offset++] = s < 0 ? s * 0x8000 : s * 0x7fff;
      }
    }
    this.chunks = [];
    return { pcm: new Uint8Array(pcm.buffer), rate };
  }
}

const recorder = new Recorder();
let recording = false;
let busy = false;
/** Where the next dictation goes. */
let target: "draft" | "note" = "draft";

function setStatus(text: string, error = false) {
  const el = $("status");
  el.textContent = text;
  el.classList.toggle("error", error);
}

function setMicState() {
  $("mic").classList.toggle("live", recording);
  $("mic").classList.toggle("busy", busy);
  $("mic").setAttribute("aria-label", recording ? "Stop and transcribe" : "Start dictating");
  const noteMic = $("note-mic");
  noteMic.textContent = recording && target === "note" ? "Stop" : busy && target === "note" ? "Working…" : "Dictate";
  noteMic.classList.toggle("live", recording && target === "note");
}

async function toggleRecording(where: "draft" | "note") {
  if (busy) return;
  if (recording) return finishRecording();
  target = where;
  try {
    await recorder.start((level) => {
      $("meter-fill").style.width = `${Math.min(100, level * 400)}%`;
    });
    recording = true;
    setStatus("Listening… tap again when you're done.");
  } catch (e) {
    setStatus(`Microphone: ${e}. Allow it under Settings → Apps → TalyxelVoice → Permissions.`, true);
  }
  setMicState();
}

async function finishRecording() {
  recording = false;
  busy = true;
  setMicState();
  $("meter-fill").style.width = "0%";
  setStatus("Transcribing…");
  const summary = (document.querySelector<HTMLInputElement>('input[name="kind"]:checked')?.value ?? "") === "summary";
  try {
    const { pcm, rate } = await recorder.stop();
    const result = await invoke<Dictated>("transcribe", pcm, {
      headers: { "x-sample-rate": String(rate), "x-summary": summary ? "1" : "0" },
    });
    if (target === "note") insertIntoNote(result.text);
    else insertAtCursor($<HTMLTextAreaElement>("draft"), result.text);
    const secs = (result.latency_ms / 1000).toFixed(1);
    if (result.warning) setStatus(`${result.warning}; kept the plain text.`, true);
    else setStatus(`Done in ${secs} s (${result.mode}).`);
  } catch (e) {
    setStatus(String(e), true);
  } finally {
    busy = false;
    setMicState();
  }
}

listen<string>("dictation-status", (e) => setStatus(`${e.payload}…`));
listen<HistoryEntry>("history-added", (e) => history.unshift(e.payload));

/** Inserts `text` at the cursor as its own sentence, with spacing that fits. */
function insertAtCursor(area: HTMLTextAreaElement, text: string) {
  const start = area.selectionStart ?? area.value.length;
  const end = area.selectionEnd ?? start;
  const before = area.value.slice(0, start);
  const after = area.value.slice(end);
  const lead = before && !/\s$/.test(before) ? " " : "";
  const trail = after && !/^\s/.test(after) ? " " : "";
  area.value = before + lead + text + trail + after;
  const cursor = (before + lead + text).length;
  area.setSelectionRange(cursor, cursor);
  saveDraft();
}

$("mic").addEventListener("click", () => toggleRecording("draft"));

// ---------------------------------------------------------------- draft

const DRAFT_KEY = "talyxel-draft";
function saveDraft() {
  try {
    localStorage.setItem(DRAFT_KEY, $<HTMLTextAreaElement>("draft").value);
  } catch {}
}
try {
  $<HTMLTextAreaElement>("draft").value = localStorage.getItem(DRAFT_KEY) ?? "";
} catch {}
$("draft").addEventListener("input", saveDraft);

$("draft-copy").addEventListener("click", () => copyText($<HTMLTextAreaElement>("draft").value));
$("draft-clear").addEventListener("click", () => {
  const draft = $<HTMLTextAreaElement>("draft");
  if (!draft.value) return;
  beforeRewrite = draft.value;
  draft.value = "";
  saveDraft();
  $("draft-undo").textContent = "Undo clear";
  $("draft-undo").hidden = false;
});
$("draft-note").addEventListener("click", async () => {
  const body = $<HTMLTextAreaElement>("draft").value.trim();
  if (!body) return toast("The draft is empty", true);
  await invoke<Note>("save_note", { note: { id: 0, title: "", body, pinned: false, created: 0, updated: 0 } });
  toast("Saved as a note");
});

let beforeRewrite: string | null = null;
$("draft-transform").addEventListener("change", async (e) => {
  const select = e.target as HTMLSelectElement;
  const transform = select.value;
  select.value = "";
  const draft = $<HTMLTextAreaElement>("draft");
  if (!transform || !draft.value.trim()) return;
  setStatus("Rewriting…");
  try {
    const text = await invoke<string>("rewrite", { text: draft.value, transform });
    beforeRewrite = draft.value;
    draft.value = text;
    saveDraft();
    $("draft-undo").textContent = "Undo rewrite";
    $("draft-undo").hidden = false;
    setStatus("Rewritten.");
  } catch (e) {
    setStatus(`Couldn't rewrite it: ${e}. Set up a text model server under Settings → Cleanup.`, true);
  }
});
$("draft-undo").addEventListener("click", () => {
  if (beforeRewrite === null) return;
  $<HTMLTextAreaElement>("draft").value = beforeRewrite;
  beforeRewrite = null;
  saveDraft();
  $("draft-undo").hidden = true;
});

// ---------------------------------------------------------------- notes

let notes: Note[] = [];
let current: Note | null = null;
let noteSaveTimer: number | undefined;

async function loadNotes() {
  notes = await invoke<Note[]>("list_notes");
}

function noteTitle(n: Note) {
  return n.title.trim() || n.body.trim().split("\n")[0].slice(0, 60) || "Untitled";
}

function renderNotes() {
  const query = input("notes-search").value.trim().toLowerCase();
  const list = $("notes");
  list.replaceChildren();
  const shown = notes
    .filter((n) => !query || `${n.title}\n${n.body}`.toLowerCase().includes(query))
    .sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.updated - a.updated);
  for (const n of shown) {
    const li = document.createElement("li");
    li.className = current?.id === n.id ? "active" : "";
    const title = document.createElement("b");
    title.textContent = (n.pinned ? "📌 " : "") + noteTitle(n);
    const meta = document.createElement("span");
    meta.className = "muted small";
    meta.textContent = new Date(n.updated * 1000).toLocaleString();
    li.append(title, meta);
    li.addEventListener("click", () => openNote(n));
    list.appendChild(li);
  }
  $("notes-empty").hidden = shown.length > 0;
  $("note-editor").hidden = current === null;
  $("note-none").hidden = current !== null;
  document.querySelector(".notes")!.classList.toggle("editing", current !== null);
}

function openNote(n: Note | null) {
  current = n;
  if (n) {
    input("note-title").value = n.title;
    $<HTMLTextAreaElement>("note-body").value = n.body;
    $("note-pin").textContent = n.pinned ? "Unpin" : "Pin";
  }
  renderNotes();
}

async function saveNote() {
  if (!current) return;
  current.title = input("note-title").value;
  current.body = $<HTMLTextAreaElement>("note-body").value;
  const saved = await invoke<Note>("save_note", { note: current });
  const fresh = current.id === 0;
  current = { ...current, ...saved, title: current.title, body: current.body };
  const i = notes.findIndex((n) => n.id === saved.id);
  if (i >= 0) notes[i] = saved;
  else notes.push(saved);
  if (fresh) renderNotes();
}

function scheduleNoteSave() {
  clearTimeout(noteSaveTimer);
  noteSaveTimer = setTimeout(() => saveNote().catch((e) => toast(String(e), true)), 500);
}

function insertIntoNote(text: string) {
  if (!current) return;
  const body = $<HTMLTextAreaElement>("note-body");
  const start = body.selectionStart ?? body.value.length;
  const end = body.selectionEnd ?? start;
  const before = body.value.slice(0, start);
  const after = body.value.slice(end);
  const lead = before && !/\s$/.test(before) ? " " : "";
  const trail = after && !/^\s/.test(after) ? " " : "";
  body.value = before + lead + text + trail + after;
  const cursor = (before + lead + text).length;
  body.setSelectionRange(cursor, cursor);
  scheduleNoteSave();
}

input("notes-search").addEventListener("input", renderNotes);
$("note-new").addEventListener("click", () => {
  openNote({ id: 0, title: "", body: "", pinned: false, created: 0, updated: 0 });
  $("note-body").focus();
});
$("note-back").addEventListener("click", async () => {
  clearTimeout(noteSaveTimer);
  if (current && (current.id !== 0 || $<HTMLTextAreaElement>("note-body").value.trim())) await saveNote();
  openNote(null);
});
$("note-title").addEventListener("input", scheduleNoteSave);
$("note-body").addEventListener("input", scheduleNoteSave);
$("note-mic").addEventListener("click", () => toggleRecording("note"));
$("note-copy").addEventListener("click", () => copyText($<HTMLTextAreaElement>("note-body").value));
$("note-pin").addEventListener("click", async () => {
  if (!current) return;
  current.pinned = !current.pinned;
  $("note-pin").textContent = current.pinned ? "Unpin" : "Pin";
  await saveNote();
  renderNotes();
});
$("note-delete").addEventListener("click", async () => {
  if (!current) return;
  if (current.id !== 0) {
    if (!confirm("Delete this note?")) return;
    await invoke("delete_note", { id: current.id });
    notes = notes.filter((n) => n.id !== current!.id);
  }
  openNote(null);
});

// ---------------------------------------------------------------- history

let history: HistoryEntry[] = [];

function renderHistory() {
  const query = input("history-search").value.trim().toLowerCase();
  const list = $("history");
  list.replaceChildren();
  const shown = history.filter((h) => !query || h.text.toLowerCase().includes(query));
  for (const h of shown) {
    const li = document.createElement("li");
    const text = document.createElement("p");
    text.textContent = h.text;
    const meta = document.createElement("div");
    meta.className = "meta";
    const info = document.createElement("span");
    info.className = "muted small";
    info.textContent = `${new Date(h.at * 1000).toLocaleString()} · ${h.mode} · ${(h.latency_ms / 1000).toFixed(1)} s`;
    const copy = document.createElement("button");
    copy.className = "btn";
    copy.textContent = "Copy";
    copy.addEventListener("click", () => copyText(h.text));
    const toDraft = document.createElement("button");
    toDraft.className = "btn ghost";
    toDraft.textContent = "To draft";
    toDraft.addEventListener("click", () => {
      insertAtCursor($<HTMLTextAreaElement>("draft"), h.text);
      showTab("dictate");
    });
    meta.append(info, toDraft, copy);
    li.append(text, meta);
    list.appendChild(li);
  }
  $("history-empty").hidden = shown.length > 0;
}

input("history-search").addEventListener("input", renderHistory);
$("history-clear").addEventListener("click", async () => {
  if (!history.length || !confirm("Clear the history?")) return;
  await invoke("clear_history");
  history = [];
  renderHistory();
});

// ---------------------------------------------------------------- settings

let settings: Settings;
let saveTimer: number | undefined;

function save() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      await invoke("save_settings", { settings });
    } catch (e) {
      toast(String(e), true);
    }
  }, 400);
}

function radio(name: string, value: string, onChange: (v: string) => void) {
  document.querySelectorAll<HTMLInputElement>(`input[name="${name}"]`).forEach((r) => {
    r.checked = r.value === value;
    r.addEventListener("change", () => r.checked && onChange(r.value));
  });
}

function bindText(id: string, get: () => string, set: (v: string) => void) {
  const el = $<HTMLInputElement | HTMLTextAreaElement>(id);
  el.value = get();
  el.addEventListener("input", () => {
    set(el.value);
    save();
  });
}

function showEngine() {
  $("engine-local").hidden = settings.engine !== "local";
  $("engine-remote").hidden = settings.engine !== "remote";
}

const downloading = new Map<string, number>();
let models: ModelInfo[] = [];

async function loadModels() {
  models = await invoke<ModelInfo[]>("list_models");
  renderModels();
}

function renderModels() {
  const list = $("models");
  list.replaceChildren();
  for (const m of models) {
    const li = document.createElement("li");
    li.className = m.name === settings.local_model ? "model selected" : "model";
    const name = document.createElement("span");
    name.innerHTML = `<b></b> <span class="muted small">${m.size_mb} MB</span>`;
    name.querySelector("b")!.textContent = m.name;
    const action = document.createElement("button");
    const progress = downloading.get(m.name);
    if (progress !== undefined) {
      action.className = "btn";
      action.disabled = true;
      action.textContent = `${progress}%`;
    } else if (!m.downloaded) {
      action.className = "btn";
      action.textContent = "Download";
      action.addEventListener("click", () => download(m.name));
    } else if (m.name === settings.local_model) {
      action.className = "btn";
      action.disabled = true;
      action.textContent = "In use";
    } else {
      action.className = "btn primary";
      action.textContent = "Use";
      action.addEventListener("click", () => {
        settings.local_model = m.name;
        save();
        renderModels();
      });
    }
    li.append(name, action);
    list.appendChild(li);
  }
}

async function download(name: string) {
  downloading.set(name, 0);
  renderModels();
  try {
    await invoke("download_model", { name, text: false });
    // The first model downloaded becomes the one in use.
    if (!models.find((m) => m.name === settings.local_model)?.downloaded) {
      settings.local_model = name;
      save();
    }
    toast(`${name} downloaded`);
  } catch (e) {
    toast(`Download failed: ${e}`, true);
  } finally {
    downloading.delete(name);
    await loadModels();
  }
}

listen<{ name: string; downloaded: number; total: number }>("model-progress", (e) => {
  const { name, downloaded, total } = e.payload;
  downloading.set(name, total ? Math.floor((downloaded / total) * 100) : 0);
  renderModels();
});

async function testConnection(command: string, remote: Remote, status: string) {
  const el = $(status);
  el.textContent = "Testing…";
  el.className = "status";
  try {
    el.textContent = await invoke<string>(command, { remote });
    el.classList.add("ok");
  } catch (e) {
    el.textContent = String(e);
    el.classList.add("bad");
  }
}

function bindSettings() {
  radio("engine", settings.engine, (v) => {
    settings.engine = v as Engine;
    showEngine();
    save();
  });
  showEngine();
  bindText("remote-url", () => settings.remote.url, (v) => (settings.remote.url = v));
  bindText("remote-model", () => settings.remote.model, (v) => (settings.remote.model = v));
  bindText("remote-key", () => settings.remote.api_key, (v) => (settings.remote.api_key = v));
  $("remote-test").addEventListener("click", () => testConnection("test_remote", settings.remote, "remote-status"));

  const language = $<HTMLSelectElement>("language");
  language.value = settings.language;
  language.addEventListener("change", () => {
    settings.language = language.value;
    save();
  });
  bindText("dictionary", () => settings.dictionary.join("\n"), (v) => (settings.dictionary = v.split("\n")));
  const punct = input("spoken-punctuation");
  punct.checked = settings.spoken_punctuation;
  punct.addEventListener("change", () => {
    settings.spoken_punctuation = punct.checked;
    save();
  });

  radio("polish", settings.polish, (v) => {
    settings.polish = v as Polish;
    save();
  });
  const text = settings.text_model.remote;
  bindText("text-url", () => text.url, (v) => (text.url = v));
  bindText("text-model", () => text.model, (v) => (text.model = v));
  bindText("text-key", () => text.api_key, (v) => (text.api_key = v));
  $("text-test").addEventListener("click", () => testConnection("test_text_remote", text, "text-status"));

  const keep = input("keep-history");
  keep.checked = settings.keep_history;
  keep.addEventListener("change", () => {
    settings.keep_history = keep.checked;
    save();
  });
}

// ---------------------------------------------------------------- start

async function init() {
  settings = await invoke<Settings>("get_settings");
  bindSettings();
  const [entries, version] = await Promise.all([
    invoke<HistoryEntry[]>("get_history"),
    invoke<string>("app_version"),
    loadModels(),
    loadNotes(),
  ]);
  history = entries;
  $("version").textContent = `TalyxelVoice ${version} for Android`;
  if (settings.engine === "local" && !models.some((m) => m.downloaded)) {
    setStatus("First, download a speech model under Settings (base or small-q5_1).", true);
  }
  setMicState();
}

init().catch((e) => toast(String(e), true));
