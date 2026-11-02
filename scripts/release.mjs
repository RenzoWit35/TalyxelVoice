// Bumps the version everywhere it lives, commits and tags it.
// Usage: npm run release -- 0.2.0   (then: git push --follow-tags)
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const version = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(version ?? "")) {
  console.error("usage: npm run release -- <major.minor.patch>");
  process.exit(1);
}

const edit = (path, fn) => writeFileSync(path, fn(readFileSync(path, "utf8")));
const json = (path) =>
  edit(path, (s) => JSON.stringify({ ...JSON.parse(s), version }, null, 2) + "\n");

json("package.json");
json("src-tauri/tauri.conf.json");
edit("src-tauri/Cargo.toml", (s) => s.replace(/^version = ".*"$/m, `version = "${version}"`));
edit("src-tauri/Cargo.lock", (s) =>
  // \r?: a Windows checkout has CRLF line endings.
  s.replace(/(name = "talyxel-voice"\r?\nversion = )".*"/, `$1"${version}"`),
);

const git = (...args) => execFileSync("git", args, { stdio: "inherit" });
git("add", "package.json", "src-tauri/tauri.conf.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock");
git("commit", "-m", `Release v${version}`);
// Annotated, because `git push --follow-tags` skips lightweight tags.
git("tag", "-a", `v${version}`, "-m", `v${version}`);
console.log(`\nTagged v${version}. Publish it with: git push --follow-tags`);
