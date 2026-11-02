// Fetches the pinned binaries bundled as resources (both gitignored). Runs
// before every dev/build; a no-op once present.
// - src-tauri/llama/: the llama.cpp Vulkan build that runs the text-polish
//   model, keeping only what llama-server needs. Without a usable GPU it runs
//   on the CPU.
// - src-tauri/vulkan/: the Vulkan loader, for PCs whose drivers didn't install
//   one; whisper's import of it is delay-loaded (src-tauri/src/gpu.rs).
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, readdirSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

const LLAMA = "b11721";
const VULKAN_RT = "1.4.363.0";
const root = join(import.meta.dirname, "..", "src-tauri");

if (process.platform !== "win32") {
  console.log("fetch-llama: not on Windows, skipping");
  process.exit(0);
}

await fetchZip({
  url: `https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA}/llama-${LLAMA}-bin-win-vulkan-x64.zip`,
  sha256: "b2dab62833cc07aae4a1f28752874668b6a98d304a7f8ca6c1421a0d1e1a7075",
  out: join(root, "llama"),
  version: `${LLAMA}-vulkan`,
  files: (dir) => {
    const keep = [
      /^llama-server\.exe$/,
      /^llama-server-impl\.dll$/,
      /^llama-common\.dll$/,
      /^llama\.dll$/,
      /^mtmd\.dll$/,
      /^ggml(-base)?\.dll$/,
      /^ggml-cpu-.+\.dll$/, // one per CPU generation; ggml picks the best at runtime
      /^ggml-vulkan\.dll$/,
      /^libomp\.dll$/,
      /^LICENSE/,
    ];
    return readdirSync(dir).filter((name) => keep.some((re) => re.test(name))).map((name) => join(dir, name));
  },
});

await fetchZip({
  url: `https://sdk.lunarg.com/sdk/download/${VULKAN_RT}/windows/VulkanRT-X64-${VULKAN_RT}-Components.zip`,
  sha256: "a25a927aa8b9f0371048f1861cf88ac3b9bc9b1fb332c42d897c8ab32695769a",
  out: join(root, "vulkan"),
  version: VULKAN_RT,
  files: (dir) => {
    const base = join(dir, `VulkanRT-X64-${VULKAN_RT}-Components`);
    return [join(base, "x64", "vulkan-1.dll"), join(base, "VulkanRT-License.txt")];
  },
});

/** Downloads `url`, checks it, and copies `files(unzippedDir)` into `out`. */
async function fetchZip({ url, sha256, out, version, files }) {
  const stamp = join(out, "BUILD");
  if (existsSync(stamp) && readFileSync(stamp, "utf8").trim() === version) return;

  console.log(`fetch-llama: downloading ${url}`);
  const res = await fetch(url);
  if (!res.ok) throw new Error(`download failed: ${res.status}`);
  const zip = Buffer.from(await res.arrayBuffer());
  const hash = createHash("sha256").update(zip).digest("hex");
  if (hash !== sha256) throw new Error(`checksum mismatch for ${url}: ${hash}`);

  const work = join(tmpdir(), `talyxel-fetch-${version}`);
  rmSync(work, { recursive: true, force: true });
  mkdirSync(work, { recursive: true });
  writeFileSync(join(work, "download.zip"), zip);
  // Windows' own bsdtar reads zips; a Git Bash tar earlier on PATH doesn't.
  const tar = join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
  execFileSync(tar, ["-xf", "download.zip"], { cwd: work, stdio: "inherit" });

  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  for (const file of files(work)) copyFileSync(file, join(out, file.split(/[\\/]/).pop()));
  writeFileSync(stamp, version + "\n");
  rmSync(work, { recursive: true, force: true });
  console.log(`fetch-llama: ${version} ready in ${out}`);
}
