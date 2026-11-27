import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { profiles, build, root } from "./windows";

export type Mode = "dev" | "stable";
export function manifest() {
  const manifest = Bun.TOML.parse(readFileSync(join(root, "Cargo.toml"), "utf8")) as { package: { name: string; version: string } };
  return manifest.package;
}

export function tagFor(mode: Mode, version: string, sha: string, supplied?: string) {
  const number = "(?:0|[1-9][0-9]*)";
  const identifier = "(?:" + number + "|[0-9]*[A-Za-z-][0-9A-Za-z-]*)";
  const semver = new RegExp("^" + number + "\\." + number + "\\." + number
    + "(?:-(" + identifier + "(?:\\." + identifier + ")*))?(?:\\+([0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*))?$");
  const match = semver.exec(version);
  if (!match || match[0] !== version) throw Error("Cargo package version is not valid semantic versioning: " + version);
  if (!/^[a-f\d]{7,64}$/i.test(sha) || sha.trim() !== sha) throw Error("release requires a hexadecimal commit SHA of at least seven characters");
  if (mode === "stable" && match[1]) throw Error("stable mode requires a normal (non-prerelease) Cargo version");
  if (mode === "dev" && match[2]) throw Error("dev release versions must not contain SemVer build metadata");
  const tag = "v" + version + (mode === "dev" ? "-dev-" + sha.slice(0, 7).toLowerCase() : "");
  if (supplied !== undefined && tag !== supplied) throw Error(`tag mismatch: expected ${tag} for ${mode} mode, got ${supplied}`);
  return tag;
}

export function assetPaths(dir: string, name: string, version: string, mode: Mode, sha: string) {
  const prefix = name + "-" + version + (mode === "dev" ? "-dev-" + sha.slice(0, 7).toLowerCase() : "") + "-";
  return [...profiles.map(profile => prefix + "windows-" + profile + ".exe"), "SHA256SUMS.txt"].map(file => join(dir, file));
}

export function digest(file: string) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

export function checksums(files: string[]) {
  return files.slice(0, -1).map(file => digest(file) + "  " + basename(file)).join("\n") + "\n";
}

export function verifyAssets(dir: string, files: string[]) {
  const actual = readdirSync(dir, { withFileTypes: true }).filter(entry => entry.isFile() || entry.isSymbolicLink()).map(entry => entry.name);
  if (actual.length !== files.length || files.some(file => !actual.includes(basename(file)))) throw Error("release asset set mismatch");
  const seen = new Set<string>();
  for (const [index, profile] of profiles.entries()) {
    const machine = profile === "arm64" ? 0xaa64 : 0x8664;
    const data = readFileSync(files[index]);
    if (data.length < 64 || data.toString("ascii", 0, 2) !== "MZ") throw Error("missing DOS header: " + basename(files[index]));
    const offset = data.readUInt32LE(60);
    if (offset < 64 || offset + 94 > data.length || data.readUInt32LE(offset) !== 0x4550 || data.readUInt16LE(offset + 4) !== machine)
      throw Error("expected " + profile + " PE header: " + basename(files[index]));
    if (data.readUInt16LE(offset + 24) !== 0x20b || data.readUInt16LE(offset + 92) !== 2)
      throw Error("unexpected PE subsystem for " + basename(files[index]));
    const stamps = profiles.filter(p => data.includes(Buffer.from("disksonar-cpu:" + p + ";")));
    if (stamps.length !== 1 || stamps[0] !== profile) throw Error("incorrect compiled CPU profile: " + basename(files[index]));
    const hash = digest(files[index]);
    if (seen.has(hash)) throw Error("duplicate executable: " + basename(files[index]));
    seen.add(hash);
  }
  const found = readFileSync(files[profiles.length], "utf8").replaceAll("\r\n", "\n").replace(/\n$/, "");
  if (checksums(files).trimEnd() !== found) throw Error("release checksum manifest does not match the three executable assets");
}

export function packageAssets(dir: string, files: string[], name: string, tag: string) {
  mkdirSync(dir, { recursive: true });
  for (const [index, profile] of profiles.entries()) copyFileSync(build(profile, name), files[index]);
  writeFileSync(files[profiles.length], checksums(files));
  verifyAssets(dir, files);
  console.log(`verified release assets for ${tag}: ${files.map(file => basename(file)).join(", ")}`);
}
