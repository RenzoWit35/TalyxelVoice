import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Engine = "local" | "remote";

interface Settings {
  hotkey: string;
  engine: Engine;
  local_model: string;
  remote: { url: string; api_key: string; model: string };
  language: string;
  spoken_languages: string[];
  dictionary: string[];
  microphone: string;
  restore_clipboard: boolean;
}

interface ModelInfo {
  name: string;
  size_mb: number;
  downloaded: boolean;
}

interface HistoryEntry {
  text: string;
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
}

function renderEngine() {
  document.querySelectorAll<HTMLInputElement>('input[name="engine"]').forEach((r) => {
    r.checked = r.value === settings.engine;
  });
  $("engine-local").hidden = settings.engine !== "local";
  $("engine-remote").hidden = settings.engine !== "remote";
}

async function renderModels() {
  const models = await invoke<ModelInfo[]>("list_models");
  const list = $("models");
  list.replaceChildren(
    ...models.map((m) => {
      const li = document.createElement("li");
      li.className = "model" + (m.name === settings.local_model ? " selected" : "");
      const info = document.createElement("div");
      info.innerHTML = `<b>${m.name}</b><span class="muted small">${m.size_mb} MB</span>`;
      const action = document.createElement("button");
      action.className = "btn";
      const pct = downloading.get(m.name);
      if (pct !== undefined) {
        action.textContent = `${pct}%`;
        action.disabled = true;
      } else if (!m.downloaded) {
        action.textContent = "Download";
        action.onclick = () => download(m.name);
      } else if (m.name === settings.local_model) {
        action.textContent = "In use";
        action.disabled = true;
      } else {
        action.textContent = "Use";
        action.onclick = () => {
          settings.local_model = m.name;
          save(true);
          renderModels();
        };
      }
      li.append(info, action);
      return li;
    }),
  );
}

async function download(name: string) {
  downloading.set(name, 0);
  renderModels();
  try {
    await invoke("download_model", { name });
    toast(`${name} downloaded`);
    const current = (await invoke<ModelInfo[]>("list_models")).find((m) => m.name === settings.local_model);
    if (!current?.downloaded) {
      settings.local_model = name;
      save(true);
    }
  } catch (e) {
    toast(`Download failed: ${e}`, true);
  } finally {
    downloading.delete(name);
    renderModels();
  }
}

function historyItem(h: HistoryEntry) {
  const li = document.createElement("li");
  const text = document.createElement("p");
  text.textContent = h.text;
  const meta = document.createElement("div");
  meta.className = "meta";
  const when = new Date(h.at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  meta.textContent = [when, h.app || "unknown app", h.engine, `${(h.latency_ms / 1000).toFixed(1)}s`].join(" · ");
  const copy = document.createElement("button");
  copy.className = "btn ghost";
  copy.textContent = "Copy";
  copy.onclick = async () => {
    await navigator.clipboard.writeText(h.text);
    toast("Copied");
  };
  meta.append(copy);
  li.append(text, meta);
  return li;
}

function renderHistory(items: HistoryEntry[]) {
  $("history").replaceChildren(...items.map(historyItem));
  $("history-empty").hidden = items.length > 0;
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

  const restore = $<HTMLInputElement>("restore-clipboard");
  restore.checked = settings.restore_clipboard;
  restore.onchange = () => ((settings.restore_clipboard = restore.checked), save(true));

  const hotkey = $<HTMLInputElement>("hotkey");
  hotkey.value = settings.hotkey;
  hotkey.addEventListener("keydown", (e) => {
    e.preventDefault();
    if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return;
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
    const previous = settings.hotkey;
    settings.hotkey = combo;
    hotkey.value = combo;
    invoke("save_settings", { settings })
      .then(() => (toast(`Hotkey set to ${combo}`), renderBadge()))
      .catch((err) => {
        settings.hotkey = previous;
        hotkey.value = previous;
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
  bindInputs();
  await Promise.all([renderModels(), loadMicrophones(), initUpdates()]);

  const history = await invoke<HistoryEntry[]>("get_history");
  renderHistory(history);
  await listen<HistoryEntry>("history-added", (e) => {
    history.unshift(e.payload);
    renderHistory(history);
  });
  await listen<{ name: string; downloaded: number; total: number }>("model-progress", (e) => {
    const { name, downloaded, total } = e.payload;
    downloading.set(name, total ? Math.floor((downloaded / total) * 100) : 0);
    renderModels();
  });
}

init().catch((e) => toast(String(e), true));
