import { listen } from "@tauri-apps/api/event";

type Kind = "dictate" | "summary" | "command";

type PillEvent =
  | { state: "listening"; hands_free: boolean; kind: Kind }
  | { state: "level"; level: number }
  | { state: "transcribing" }
  | { state: "polishing"; summary: boolean }
  | { state: "working"; message: string }
  | { state: "info"; message: string }
  | { state: "error"; message: string };

const BAR_COUNT = 9;
const pill = document.getElementById("pill")!;
const label = document.getElementById("label")!;
const barsEl = document.getElementById("bars")!;
const bars = Array.from({ length: BAR_COUNT }, () => barsEl.appendChild(document.createElement("i")));
const history: number[] = new Array(BAR_COUNT).fill(0);

const LISTENING: Record<Kind, [string, string]> = {
  dictate: ["Listening", "Hands-free · tap to stop"],
  summary: ["Summary · listening", "Summary · tap to stop"],
  command: ["Say what to do with the selection", "Command · tap to stop"],
};

function setLevel(rms: number) {
  // Speech RMS sits around 0.01–0.2; map it onto a visible range.
  const v = Math.min(1, Math.sqrt(rms) * 2.4);
  history.shift();
  history.push(v);
  bars.forEach((b, i) => (b.style.height = `${4 + history[i] * 16}px`));
}

function busy(text: string) {
  pill.dataset.state = "transcribing";
  label.textContent = text;
  pill.title = text;
  bars.forEach((b) => (b.style.height = ""));
}

listen<PillEvent>("pill", ({ payload }) => {
  pill.title = "";
  switch (payload.state) {
    case "listening":
      pill.dataset.state = "listening";
      label.textContent = LISTENING[payload.kind][payload.hands_free ? 1 : 0];
      history.fill(0);
      break;
    case "level":
      setLevel(payload.level);
      break;
    case "transcribing":
      busy("Transcribing");
      break;
    case "polishing":
      busy(payload.summary ? "Summarizing" : "Formatting");
      break;
    case "working":
      busy(payload.message);
      break;
    case "info":
      pill.dataset.state = "info";
      label.textContent = payload.message;
      pill.title = payload.message;
      break;
    case "error":
      pill.dataset.state = "error";
      label.textContent = payload.message;
      pill.title = payload.message;
      break;
  }
});
