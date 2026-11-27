import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { digest, verifyAssets } from "./assets";
import { repository, select, type Api, type Selection } from "./channels";

const maxSize = 128 * 1024 * 1024;
function gh(route: string, flags: string[] = []) {
  try {
    return execFileSync("gh", ["api", ...flags, "repos/" + repository + "/" + route],
      { maxBuffer: maxSize, stdio: ["ignore", "pipe", "pipe"] });
  } catch { throw Error("GitHub request failed: " + route + " (check read access and availability)"); }
}

const api: Api = {
  get: route => JSON.parse(gh(route).toString()),
  pages<T>(route: string, key?: string): T[] {
    const result: T[] = [];
    for (let page = 1; ; page++) {
      const response = JSON.parse(gh(route + "&page=" + page).toString());
      const items = key ? response[key] : response;
      if (!Array.isArray(items)) throw Error("Invalid paginated response");
      result.push(...items);
      if (items.length < 100) return result;
    }
  },
};

function download(selection: Selection, dir: string) {
  if (selection.artifact) {
    const zip = join(dir, "distribution.zip");
    const bytes = gh("actions/artifacts/" + selection.artifact.id + "/zip");
    if ("sha256:" + createHash("sha256").update(bytes).digest("hex") !== selection.artifact.digest)
      throw Error("GitHub artifact digest mismatch");
    writeFileSync(zip, bytes);
    const windows = process.platform === "win32";
    const tool = windows ? "tar.exe" : "unzip";
    const listing = execFileSync(tool, windows ? ["-tf", zip] : ["-Z1", zip], { encoding: "utf8", maxBuffer: 16384 })
      .trimEnd().split(/\r?\n/);
    if (listing.length !== selection.names.length || !selection.names.every(n => listing.includes(n)))
      throw Error("Unexpected archive entries");
    // Extract only individual allowlisted members to stdout. Never extract paths
    // or links from an untrusted archive into the user's filesystem.
    for (const name of selection.names) {
      const data = execFileSync(tool, windows ? ["-xOf", zip, name] : ["-p", zip, name], { maxBuffer: maxSize });
      writeFileSync(join(dir, name), data);
    }
    rmSync(zip);
  } else {
    for (const asset of selection.assets!) {
      const bytes = gh("releases/assets/" + asset.id, ["-H", "Accept: application/octet-stream"]);
      if ("sha256:" + createHash("sha256").update(bytes).digest("hex") !== asset.digest) throw Error("Asset digest mismatch: " + asset.name);
      writeFileSync(join(dir, asset.name), bytes);
    }
  }
  verifyAssets(dir, selection.names.map(n => join(dir, n)));
}

export function checkCurrent(before: Selection, after: Selection) {
  if (JSON.stringify(before) !== JSON.stringify(after)) throw Error("Channel changed during installation; retry");
}

function install(args: string[]) {
  const [channel, ui, profile] = args;
  if (args.length !== 3 || !["gui", "tui"].includes(ui) || !["x64", "x64-baseline", "arm64"].includes(profile))
    throw Error("Usage: mise run channel-install CHANNEL gui|tui arm64|x64|x64-baseline");
  const selected = select(api, channel);
  const key = [selected.sha, selected.artifact?.id ?? selected.release, ui, profile].join("-");
  const store = join(process.env.LOCALAPPDATA ?? join(homedir(), ".local", "share"), "chkdsk", "packages");
  const target = join(store, key);
  const temp = mkdtempSync(join(tmpdir(), "chkdsk-download-"));
  try {
    download(selected, temp);
    const name = selected.names.find(n => n.endsWith("-" + ui + "-windows-" + profile + ".exe"))!;
    const source = join(temp, name);
    const binName = "chkdsk-" + ui + ".exe";
    mkdirSync(store, { recursive: true });
    const staging = mkdtempSync(join(store, ".install-"));
    try {
      mkdirSync(join(staging, "bin"));
      writeFileSync(join(staging, "bin", binName), readFileSync(source), {mode: 0o755});
      checkCurrent(selected, select(api, channel));
      try { renameSync(staging, target); }
      catch (error) {
        if (!existsSync(target) || digest(join(target, "bin", binName)) !== digest(source)) throw error;
      }
    } finally { rmSync(staging, { recursive: true, force: true }); }
    const tool = "github:" + repository + "@" + key;
    // Do not replace an unrelated installation, even if another process won the race.
    const existing = Bun.spawnSync(["mise", "where", tool]);
    if (existing.exitCode === 0) {
      if (realpathSync(existing.stdout.toString().trim()) !== realpathSync(target)) throw Error("Conflicting mise installation");
    } else execFileSync("mise", ["link", tool, target], { stdio: "inherit" });
    console.log(selected.channel, selected.sha, name, digest(source));
    console.log("mise x " + tool + " -- " + binName);
  } finally { rmSync(temp, { recursive: true, force: true }); }
}
if (import.meta.main) {
  try { install(process.argv.slice(2)); }
  catch (error) { console.error(error instanceof Error ? error.message : error); process.exitCode = 1; }
}
