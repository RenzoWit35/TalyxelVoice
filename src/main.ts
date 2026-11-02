import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Engine = "local" | "remote";
type Polish = "off" | "clean" | "medium" | "format";
type FormatStyle = "text" | "note" | "list" | "timelog";

interface Remote {
  url: string;
  api_key: string;
  model: string;
}

interface Settings {
  hotkey: string;
  summary_hotkey: string;
  undo_hotkey: string;
  repaste_hotkey: string;
  polish: Polish;
  format_style: FormatStyle;
  app_modes: AppMode[];
  text_model: { engine: Engine; local_model: string; remote: Remote };
  engine: Engine;
  local_model: string;
  remote: Remote;
  language: string;
  spoken_languages: string[];
  dictionary: string[];
  spoken_punctuation: boolean;
  snippets: Snippet[];
  microphone: string;
  restore_clipboard: boolean;
  autostart: boolean;
  keep_history: boolean;
  history_days: number;
}

type HotkeyField = "hotkey" | "summary_hotkey" | "undo_hotkey" | "repaste_hotkey";

interface Snippet {
  trigger: string;
  text: string;
}

interface AppMode {
  exe: string;
  polish: Polish;
  format_style: FormatStyle;
}

interface ModelInfo {
  name: string;
  size_mb: number;
  note: string;
  downloaded: boolean;
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

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let settings: Settings;
const downloading = new Map<string, number>();

function toast(message: string, error = false) {
  const el = $("toast");
  el.textContent = message;
  el.classList.toggle("error", error);
  el.classList.add("show");
  clearTimeout((el as any)._t);
  (el as any)._t = setTimeout(() => el.classList.remove("show"), error ? 4000 : 1400);
}

let saveTimer: number | undefined;
function save(immediate = false) {
  clearTimeout(saveTimer);
  const run = async () => {
    try {
      await invoke("save_settings", { settings });
      toast("Saved");
      renderBadge();
    } catch (e) {
      toast(String(e), true);
    }
  };
  if (immediate) run();
  else saveTimer = window.setTimeout(run, 500);
}

function splitList(value: string, sep: RegExp) {
  return value.split(sep).map((s) => s.trim()).filter(Boolean);
}

// --- tabs ---
document.querySelectorAll<HTMLButtonElement>(".nav").forEach((btn) =>
  btn.addEventListener("click", () => {
    document.querySelectorAll(".nav").forEach((b) => b.classList.toggle("active", b === btn));
    document.querySelectorAll(".tab").forEach((t) =>
      t.classList.toggle("active", t.id === `tab-${btn.dataset.tab}`),
    );
  }),
);

// --- rendering ---
function renderBadge() {
  const label =
    settings.engine === "local" ? `On device · ${settings.local_model}` : `Server · ${settings.remote.model || "default"}`;
  $("engine-badge").textContent = label;
  $("hotkey-hint").textContent = settings.hotkey;
  $("summary-hint").textContent = settings.summary_hotkey || "(off)";
  $("undo-hint").textContent = settings.undo_hotkey || "(off)";
}

const POLISH_HINTS: Record<Polish, string> = {
  off: "Pastes exactly what whisper heard.",
  clean: "Removes filler words (uh, um, eh) and stutters like \"we should, we should\". Instant, no model needed.",
  medium:
    "Cleans up, then the text model fixes punctuation and keeps only your correction when you say \"Thursday, no wait, Friday\". Your words stay yours. Takes a few seconds; works best with the 3B model or a server.",
  format: "Cleans up, then the text model reshapes it in the style below. Takes a few seconds.",
};

const POLISH_LABELS: Record<Polish, string> = { off: "As spoken", clean: "Clean up", medium: "Medium", format: "Format" };
const FORMAT_LABELS: Record<FormatStyle, string> = { text: "Text", note: "Note", list: "List", timelog: "Time log" };

function select<T extends string>(labels: Record<T, string>, value: T, onchange: (v: T) => void) {
  const el = document.createElement("select");
  for (const [v, label] of Object.entries(labels) as [T, string][]) el.add(new Option(label, v));
  el.value = value;
  el.onchange = () => onchange(el.value as T);
  return el;
}

function renderAppModes() {
  const known = new Set(history.map((h) => h.app).filter(Boolean));
  $("known-apps").replaceChildren(...[...known].sort().map((a) => new Option(a)));
  $("app-modes").replaceChildren(
    ...settings.app_modes.map((m, i) => {
      const li = document.createElement("li");
      const exe = document.createElement("input");
      exe.placeholder = "OUTLOOK.EXE";
      exe.setAttribute("list", "known-apps");
      exe.value = m.exe;
      exe.oninput = () => ((m.exe = exe.value.trim()), save());
      const style = select(FORMAT_LABELS, m.format_style, (v) => ((m.format_style = v), save(true)));
      style.hidden = m.polish !== "format";
      const polish = select(POLISH_LABELS, m.polish, (v) => {
        m.polish = v;
        style.hidden = v !== "format";
        save(true);
      });
      const remove = document.createElement("button");
      remove.className = "btn ghost";
      remove.textContent = "Remove";
      remove.onclick = () => {
        settings.app_modes.splice(i, 1);
        renderAppModes();
        save(true);
      };
      li.append(exe, polish, style, remove);
      if (!m.exe) queueMicrotask(() => exe.focus());
      return li;
    }),
  );
}

const FORMAT_STYLE_HINTS: Record<FormatStyle, string> = {
  text: "Paragraphs, with a list where you sum things up. Good for emails and messages.",
  note: "A short, businesslike note: names, numbers and agreements, without the chatter.",
  list: "Everything as bullets, with a heading if you say what the list is for.",
  timelog: "Time spent and subject first (only if you say the time), then what you did, in the past tense. For hour logs and tickets.",
};

function renderPolish() {
  document.querySelectorAll<HTMLInputElement>('input[name="polish"]').forEach((r) => {
    r.checked = r.value === settings.polish;
  });
  $("polish-hint").textContent = POLISH_HINTS[settings.polish];
  $("format-styles").hidden = settings.polish !== "format";
  document.querySelectorAll<HTMLInputElement>('input[name="format-style"]').forEach((r) => {
    r.checked = r.value === settings.format_style;
  });
  $("format-style-hint").textContent = FORMAT_STYLE_HINTS[settings.format_style];
  document.querySelectorAll<HTMLInputElement>('input[name="text-engine"]').forEach((r) => {
    r.checked = r.value === settings.text_model.engine;
  });
  $("text-local").hidden = settings.text_model.engine !== "local";
  $("text-remote").hidden = settings.text_model.engine !== "remote";
}

function renderEngine() {
  document.querySelectorAll<HTMLInputElement>('input[name="engine"]').forEach((r) => {
    r.checked = r.value === settings.engine;
  });
  $("engine-local").hidden = settings.engine !== "local";
  $("engine-remote").hidden = settings.engine !== "remote";
}

async function renderModels() {
  await Promise.all([renderModelList(false), renderModelList(true)]);
}

async function renderModelList(text: boolean) {
  const models = await invoke<ModelInfo[]>(text ? "list_text_models" : "list_models");
  const list = $(text ? "text-models" : "models");
  const current = () => (text ? settings.text_model.local_model : settings.local_model);
  list.replaceChildren(
    ...models.map((m) => {
      const li = document.createElement("li");
      li.className = "model" + (m.name === current() ? " selected" : "");
      const info = document.createElement("div");
      const size = m.size_mb >= 1000 ? `${(m.size_mb / 1000).toFixed(1)} GB` : `${m.size_mb} MB`;
      info.innerHTML = `<b>${m.name}<span class="muted small">${size}</span></b>`;
      if (m.note) {
        const note = document.createElement("span");
        note.className = "note muted small";
        note.textContent = m.note;
        info.append(note);
      }
      const action = document.createElement("button");
      action.className = "btn";
      const pct = downloading.get(m.name);
      if (pct !== undefined) {
        action.textContent = `${pct}%`;
        action.disabled = true;
      } else if (!m.downloaded) {
        action.textContent = "Download";
        action.onclick = () => download(m.name, text);
      } else if (m.name === current()) {
        action.textContent = "In use";
        action.disabled = true;
      } else {
        action.textContent = "Use";
        action.onclick = () => {
          if (text) settings.text_model.local_model = m.name;
          else settings.local_model = m.name;
          save(true);
          renderModels();
        };
      }
      li.append(info, action);
      return li;
    }),
  );
}

async function download(name: string, text: boolean) {
  downloading.set(name, 0);
  renderModels();
  try {
    await invoke("download_model", { name, text });
    toast(`${name} downloaded`);
    if (!text) {
      const current = (await invoke<ModelInfo[]>("list_models")).find((m) => m.name === settings.local_model);
      if (!current?.downloaded) {
        settings.local_model = name;
        save(true);
      }
    } else if (settings.text_model.local_model !== name) {
      settings.text_model.local_model = name;
      save(true);
    }
  } catch (e) {
    toast(`Download failed: ${e}`, true);
  } finally {
    downloading.delete(name);
    renderModels();
  }
}

function when(at: number) {
  const d = new Date(at * 1000);
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return d.toDateString() === new Date().toDateString()
    ? time
    : `${d.toLocaleDateString([], { day: "numeric", month: "short" })} ${time}`;
}

function copyButton(label: string, text: string) {
  const btn = document.createElement("button");
  btn.className = "btn ghost";
  btn.textContent = label;
  btn.onclick = async () => {
    await navigator.clipboard.writeText(text);
    toast("Copied");
  };
  return btn;
}

function historyItem(h: HistoryEntry) {
  const li = document.createElement("li");
  const text = document.createElement("p");
  text.textContent = h.text;
  const meta = document.createElement("div");
  meta.className = "meta";
  const info = document.createElement("span");
  const mode = document.createElement("span");
  mode.className = "mode";
  mode.textContent = h.mode;
  info.append(
    [when(h.at), h.app || "unknown app", h.engine, `${(h.latency_ms / 1000).toFixed(1)}s`].join(" · ") + " · ",
    mode,
  );
  const actions = document.createElement("span");
  actions.className = "row";
  actions.append(copyButton("Copy", h.text));
  li.append(text);
  const changed = h.raw && h.raw !== h.text;
  if (changed) {
    const original = document.createElement("p");
    original.className = "original";
    original.textContent = h.raw;
    original.hidden = true;
    const toggle = document.createElement("button");
    toggle.className = "btn ghost";
    toggle.textContent = "Original";
    toggle.onclick = () => {
      original.hidden = !original.hidden;
      toggle.textContent = original.hidden ? "Original" : "Hide original";
    };
    actions.prepend(toggle, copyButton("Copy original", h.raw));
    li.append(original);
  }
  meta.append(info, actions);
  li.append(meta);
  return li;
}

let history: HistoryEntry[] = [];

function renderSnippets() {
  const list = $("snippets");
  list.replaceChildren(
    ...settings.snippets.map((s, i) => {
      const li = document.createElement("li");
      const trigger = document.createElement("label");
      trigger.className = "field";
      trigger.textContent = "When I say";
      const triggerInput = document.createElement("input");
      triggerInput.placeholder = "my signature";
      triggerInput.value = s.trigger;
      triggerInput.oninput = () => ((s.trigger = triggerInput.value), save());
      trigger.append(triggerInput);
      const remove = document.createElement("button");
      remove.className = "btn ghost";
      remove.textContent = "Remove";
      remove.onclick = () => {
        settings.snippets.splice(i, 1);
        renderSnippets();
        save(true);
      };
      const text = document.createElement("textarea");
      text.placeholder = "Kind regards,\nRenzo";
      text.value = s.text;
      text.oninput = () => ((s.text = text.value), save());
      li.append(trigger, remove, text);
      if (!s.trigger) queueMicrotask(() => triggerInput.focus());
      return li;
    }),
  );
}

function renderHistory() {
  const q = $<HTMLInputElement>("history-search").value.trim().toLowerCase();
  const items = q
    ? history.filter((h) => h.text.toLowerCase().includes(q) || h.raw.toLowerCase().includes(q) || h.app.toLowerCase().includes(q))
    : history;
  $("history").replaceChildren(...items.map(historyItem));
  $("history-empty").hidden = items.length > 0;
  $("history-empty").textContent = q
    ? "No dictation matches that."
    : "Nothing yet. Put your cursor in any text field and try the hotkey.";
  renderStats();
}

// Today's words and the average time per dictation, so local and server can be compared.
function renderStats() {
  const start = new Date();
  start.setHours(0, 0, 0, 0);
  const today = history.filter((h) => h.at * 1000 >= start.getTime());
  if (today.length === 0) {
    $("history-stats").textContent = "";
    return;
  }
  const words = today.reduce((n, h) => n + h.text.split(/\s+/).filter(Boolean).length, 0);
  const avg = today.reduce((n, h) => n + h.latency_ms, 0) / today.length / 1000;
  $("history-stats").textContent = `Today: ${today.length} dictation${today.length === 1 ? "" : "s"}, ${words} words, ${avg.toFixed(1)}s avg`;
}

// --- inputs ---
function bindInputs() {
  document.querySelectorAll<HTMLInputElement>('input[name="engine"]').forEach((r) =>
    r.addEventListener("change", () => {
      settings.engine = r.value as Engine;
      renderEngine();
      save(true);
    }),
  );

  const url = $<HTMLInputElement>("remote-url");
  const model = $<HTMLInputElement>("remote-model");
  const key = $<HTMLInputElement>("remote-key");
  url.value = settings.remote.url;
  model.value = settings.remote.model;
  key.value = settings.remote.api_key;
  url.oninput = () => ((settings.remote.url = url.value.trim()), save());
  model.oninput = () => ((settings.remote.model = model.value.trim()), save());
  key.oninput = () => ((settings.remote.api_key = key.value.trim()), save());

  $("remote-test").onclick = async () => {
    const status = $("remote-status");
    status.className = "status";
    status.textContent = "Testing…";
    try {
      status.textContent = await invoke<string>("test_remote", { remote: settings.remote });
      status.classList.add("ok");
    } catch (e) {
      status.textContent = String(e);
      status.classList.add("bad");
    }
  };

  const language = $<HTMLSelectElement>("language");
  language.value = settings.language;
  language.onchange = () => ((settings.language = language.value), save(true));

  const spoken = $<HTMLInputElement>("spoken-languages");
  spoken.value = settings.spoken_languages.join(", ");
  spoken.oninput = () => ((settings.spoken_languages = splitList(spoken.value, /,/)), save());

  const dictionary = $<HTMLTextAreaElement>("dictionary");
  dictionary.value = settings.dictionary.join("\n");
  dictionary.oninput = () => ((settings.dictionary = splitList(dictionary.value, /\n/)), save());

  const punctuation = $<HTMLInputElement>("spoken-punctuation");
  punctuation.checked = settings.spoken_punctuation;
  punctuation.onchange = () => ((settings.spoken_punctuation = punctuation.checked), save(true));

  $("app-mode-add").onclick = () => {
    settings.app_modes.push({ exe: "", polish: "format", format_style: "text" });
    renderAppModes();
  };

  renderSnippets();
  $("snippet-add").onclick = () => {
    settings.snippets.push({ trigger: "", text: "" });
    renderSnippets();
  };

  const restore = $<HTMLInputElement>("restore-clipboard");
  restore.checked = settings.restore_clipboard;
  restore.onchange = () => ((settings.restore_clipboard = restore.checked), save(true));

  const autostart = $<HTMLInputElement>("autostart");
  autostart.checked = settings.autostart;
  autostart.onchange = () => ((settings.autostart = autostart.checked), save(true));

  const keep = $<HTMLInputElement>("keep-history");
  keep.checked = settings.keep_history;
  keep.onchange = () => ((settings.keep_history = keep.checked), save(true));
  const days = $<HTMLInputElement>("history-days");
  days.value = String(settings.history_days);
  days.oninput = () => {
    const n = Math.max(0, Math.floor(Number(days.value) || 0));
    settings.history_days = n;
    save();
  };
  $("history-search").oninput = () => renderHistory();
  $("history-clear").onclick = async () => {
    if (history.length === 0) return;
    if (!confirm(`Forget all ${history.length} dictations? This can't be undone.`)) return;
    await invoke("clear_history");
    history = [];
    renderHistory();
    toast("History cleared");
  };

  bindHotkey("hotkey", "hotkey");
  bindHotkey("summary-hotkey", "summary_hotkey");
  bindHotkey("undo-hotkey", "undo_hotkey");
  bindHotkey("repaste-hotkey", "repaste_hotkey");

  document.querySelectorAll<HTMLInputElement>('input[name="polish"]').forEach((r) =>
    r.addEventListener("change", () => {
      settings.polish = r.value as Polish;
      renderPolish();
      save(true);
    }),
  );
  document.querySelectorAll<HTMLInputElement>('input[name="format-style"]').forEach((r) =>
    r.addEventListener("change", () => {
      settings.format_style = r.value as FormatStyle;
      renderPolish();
      save(true);
    }),
  );
  document.querySelectorAll<HTMLInputElement>('input[name="text-engine"]').forEach((r) =>
    r.addEventListener("change", () => {
      settings.text_model.engine = r.value as Engine;
      renderPolish();
      save(true);
    }),
  );
  const textRemote = settings.text_model.remote;
  for (const [id, field] of [["text-url", "url"], ["text-model", "model"], ["text-key", "api_key"]] as const) {
    const input = $<HTMLInputElement>(id);
    input.value = textRemote[field];
    input.oninput = () => ((textRemote[field] = input.value.trim()), save());
  }
  $("text-test").onclick = async () => {
    const status = $("text-status");
    status.className = "status";
    status.textContent = "Testing…";
    try {
      status.textContent = await invoke<string>("test_text_remote", { remote: textRemote });
      status.classList.add("ok");
    } catch (e) {
      status.textContent = String(e);
      status.classList.add("bad");
    }
  };
}

function bindHotkey(id: string, key: HotkeyField) {
  const hotkey = $<HTMLInputElement>(id);
  const show = () => (hotkey.value = settings[key] || "(off)");
  show();
  hotkey.addEventListener("keydown", (e) => {
    e.preventDefault();
    if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return;
    if ((e.key === "Backspace" || e.key === "Delete") && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
      if (key === "hotkey") {
        toast("The dictation hotkey can't be turned off", true);
        return;
      }
      const previous = settings[key];
      settings[key] = "";
      show();
      invoke("save_settings", { settings })
        .then(() => (toast("Hotkey turned off"), renderBadge()))
        .catch((err) => ((settings[key] = previous), show(), toast(String(err), true)));
      return;
    }
    const parts = [];
    if (e.ctrlKey) parts.push("Ctrl");
    if (e.altKey) parts.push("Alt");
    if (e.shiftKey) parts.push("Shift");
    if (e.metaKey) parts.push("Super");
    if (parts.length === 0) {
      toast("Use at least one modifier (Ctrl, Alt, Shift or Win)", true);
      return;
    }
    parts.push(e.code.replace(/^Key/, "").replace(/^Digit/, ""));
    const combo = parts.join("+");
    const previous = settings[key];
    settings[key] = combo;
    show();
    invoke("save_settings", { settings })
      .then(() => (toast(`Hotkey set to ${combo}`), renderBadge()))
      .catch((err) => {
        settings[key] = previous;
        show();
        toast(String(err), true);
      });
  });
}

type UpdateStatus =
  | { state: "idle" | "checking" | "uptodate" }
  | { state: "downloading"; version: string; percent: number }
  | { state: "ready"; version: string; notes: string }
  | { state: "error"; message: string };

function renderUpdate(s: UpdateStatus) {
  const status = $("update-status");
  status.className = "status";
  const ready = s.state === "ready";
  $("update-install").hidden = !ready;
  $<HTMLButtonElement>("update-check").disabled = s.state === "checking" || s.state === "downloading";
  $("update-banner").hidden = !ready;
  switch (s.state) {
    case "idle":
      status.textContent = "";
      break;
    case "checking":
      status.textContent = "Checking…";
      break;
    case "uptodate":
      status.textContent = "You're up to date";
      status.classList.add("ok");
      break;
    case "downloading":
      status.textContent = `Downloading ${s.version}… ${s.percent}%`;
      break;
    case "ready":
      status.textContent = `${s.version} is ready`;
      status.classList.add("ok");
      $("update-banner-text").textContent = `TalyxelVoice ${s.version} is downloaded and ready to install.`;
      break;
    case "error":
      status.textContent = `Couldn't check for updates: ${s.message}`;
      status.classList.add("bad");
      break;
  }
}

async function initUpdates() {
  $("app-version").textContent = `v${await invoke<string>("app_version")}`;
  renderUpdate(await invoke<UpdateStatus>("get_update_status"));
  await listen<UpdateStatus>("update-status", (e) => renderUpdate(e.payload));
  $("update-check").onclick = () => invoke("check_for_updates");
  const install = () => invoke("install_update").catch((e) => toast(String(e), true));
  $("update-install").onclick = install;
  $("update-banner-install").onclick = install;
}

async function loadMicrophones() {
  const select = $<HTMLSelectElement>("microphone");
  const mics = await invoke<string[]>("list_microphones");
  for (const name of mics) select.add(new Option(name, name));
  select.value = settings.microphone;
  select.onchange = () => ((settings.microphone = select.value), save(true));
}

async function init() {
  settings = await invoke<Settings>("get_settings");
  renderBadge();
  renderEngine();
  renderPolish();
  bindInputs();
  await Promise.all([renderModels(), loadMicrophones(), initUpdates()]);

  history = await invoke<HistoryEntry[]>("get_history");
  renderHistory();
  renderAppModes();
  await listen<HistoryEntry>("history-added", (e) => {
    history.unshift(e.payload);
    renderHistory();
    renderAppModes();
  });
  await listen<{ name: string; downloaded: number; total: number }>("model-progress", (e) => {
    const { name, downloaded, total } = e.payload;
    downloading.set(name, total ? Math.floor((downloaded / total) * 100) : 0);
    renderModels();
  });
}

init().catch((e) => toast(String(e), true));
