// Fetches the pinned llama.cpp CPU build that runs the text-polish model, and
// keeps only what llama-server needs. Output: src-tauri/llama/ (gitignored,
// bundled as a resource). Runs before every dev/build; a no-op once present.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, readdirSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

const BUILD = "b11721";
const ZIP = `llama-${BUILD}-bin-win-cpu-x64.zip`;
const SHA256 = "19f5a91cfbd906fadfb36975a65a0c165c7db1a533d5db129dfa0a533dfdeaa1";
const KEEP = [
  /^llama-server\.exe$/,
  /^llama-server-impl\.dll$/,
  /^llama-common\.dll$/,
  /^llama\.dll$/,
  /^mtmd\.dll$/,
  /^ggml(-base)?\.dll$/,
  /^ggml-cpu-.+\.dll$/, // one per CPU generation; ggml picks the best at runtime
  /^libomp\.dll$/,
  /^LICENSE/,
];

const out = join(import.meta.dirname, "..", "src-tauri", "llama");
const stamp = join(out, "BUILD");

if (process.platform !== "win32") {
  console.log("fetch-llama: not on Windows, skipping");
  process.exit(0);
}
if (existsSync(stamp) && readFileSync(stamp, "utf8").trim() === BUILD) process.exit(0);

const url = `https://github.com/ggml-org/llama.cpp/releases/download/${BUILD}/${ZIP}`;
console.log(`fetch-llama: downloading ${url}`);
const res = await fetch(url);
if (!res.ok) throw new Error(`download failed: ${res.status}`);
const zip = Buffer.from(await res.arrayBuffer());
const hash = createHash("sha256").update(zip).digest("hex");
if (hash !== SHA256) throw new Error(`checksum mismatch for ${ZIP}: ${hash}`);

const work = join(tmpdir(), `talyxel-llama-${BUILD}`);
rmSync(work, { recursive: true, force: true });
mkdirSync(work, { recursive: true });
writeFileSync(join(work, ZIP), zip);
// Windows' own bsdtar reads zips; a Git Bash tar earlier on PATH doesn't.
const tar = join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
execFileSync(tar, ["-xf", ZIP], { cwd: work, stdio: "inherit" });

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
for (const name of readdirSync(work)) {
  if (KEEP.some((re) => re.test(name))) copyFileSync(join(work, name), join(out, name));
}
writeFileSync(stamp, BUILD + "\n");
rmSync(work, { recursive: true, force: true });
console.log(`fetch-llama: llama.cpp ${BUILD} ready in src-tauri/llama`);
