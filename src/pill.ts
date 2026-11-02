import { listen } from "@tauri-apps/api/event";

type PillEvent =
  | { state: "listening"; hands_free: boolean; summary: boolean }
  | { state: "level"; level: number }
  | { state: "transcribing" }
  | { state: "polishing"; summary: boolean }
  | { state: "info"; message: string }
  | { state: "error"; message: string };

const BAR_COUNT = 9;
const pill = document.getElementById("pill")!;
const label = document.getElementById("label")!;
const barsEl = document.getElementById("bars")!;
const bars = Array.from({ length: BAR_COUNT }, () => barsEl.appendChild(document.createElement("i")));
const history: number[] = new Array(BAR_COUNT).fill(0);

function setLevel(rms: number) {
  // Speech RMS sits around 0.01–0.2; map it onto a visible range.
  const v = Math.min(1, Math.sqrt(rms) * 2.4);
  history.shift();
  history.push(v);
  bars.forEach((b, i) => (b.style.height = `${4 + history[i] * 16}px`));
}

listen<PillEvent>("pill", ({ payload }) => {
  switch (payload.state) {
    case "listening":
      pill.dataset.state = "listening";
      label.textContent = payload.summary
        ? payload.hands_free ? "Summary · tap to stop" : "Summary · listening"
        : payload.hands_free ? "Hands-free · tap to stop" : "Listening";
      history.fill(0);
      break;
    case "level":
      setLevel(payload.level);
      break;
    case "transcribing":
      pill.dataset.state = "transcribing";
      label.textContent = "Transcribing";
      bars.forEach((b) => (b.style.height = ""));
      break;
    case "polishing":
      pill.dataset.state = "transcribing";
      label.textContent = payload.summary ? "Summarizing" : "Formatting";
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
