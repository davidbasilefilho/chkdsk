import { afterEach, beforeEach, expect, test } from "bun:test";
import { copyFileSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { assetPaths, checksums, digest, tagFor, verifyAssets } from "./assets";
import { publishRelease, type Release } from "./github";
import { CommandError, linkerFor, nativeTools, profiles, targets } from "./windows";

const sha = "abcdef0123456789";
const tag = "v1.2.3-dev-abcdef0";
let dir: string, files: string[];

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "disksonar-test-"));
  files = assetPaths(dir, "disksonar", "1.2.3", "dev", sha);
  for (const [index, profile] of profiles.entries()) {
    const pe = Buffer.alloc(256);
    pe.write("MZ"); pe.writeUInt32LE(64, 60); pe.write("PE", 64); pe.writeUInt16LE(profile === "arm64" ? 0xaa64 : 0x8664, 68);
    pe.writeUInt16LE(0x20b, 88); pe.writeUInt16LE(2, 156);
    pe.write("disksonar-cpu:" + profile + ";", 180);
    writeFileSync(files[index], pe);
  }
  writeFileSync(files[profiles.length], checksums(files));
});
afterEach(() => rmSync(dir, { recursive: true, force: true }));

test("MSVC host and target linker paths and triples", () => {
  expect(targets).toEqual({ x64: "x86_64-pc-windows-msvc", arm64: "aarch64-pc-windows-msvc" });
  for (const arch of ["x64", "arm64"] as const)
    expect(linkerFor(arch, "VC")).toBe(join("VC", "bin", "Hostx64", arch, "link.exe"));
});

test("host and target C toolchains stay distinct during ARM64 builds", () => {
  const env = { ...nativeTools("x64", "VC"), ...nativeTools("arm64", "VC") };
  for (const [arch, triple] of [["x64", "x86_64_pc_windows_msvc"], ["arm64", "aarch64_pc_windows_msvc"]]) {
    const dir = join("VC", "bin", "Hostx64", arch);
    expect(env["CC_" + triple]).toBe(join(dir, "cl.exe"));
    expect(env["CXX_" + triple]).toBe(join(dir, "cl.exe"));
    expect(env["AR_" + triple]).toBe(join(dir, "lib.exe"));
    expect(env["CARGO_TARGET_" + triple.toUpperCase() + "_LINKER"]).toBe(join(dir, "link.exe"));
  }
});

test("tags, filenames, SHA and SemVer", () => {
  expect(tagFor("dev", "1.2.3", sha.toUpperCase())).toBe(tag);
  expect(tagFor("dev", "1.2.3-rc.2", sha)).toBe("v1.2.3-rc.2-dev-abcdef0");
  expect(tagFor("stable", "1.2.3", sha)).toBe("v1.2.3");
  for (const value of ["01.2.3", "1.2", "1.2.3-01", "1.2.3-rc.1"]) expect(() => tagFor("stable", value, sha)).toThrow();
  expect(() => tagFor("stable", "1.2.3", sha, "v1.2.4")).toThrow();
  expect(() => tagFor("dev", "1.2.3+meta", sha)).toThrow();
  expect(() => tagFor("dev", "1.2.3", "no-sha")).toThrow();
  for (const mode of ["dev", "stable"] as const) {
    const prefix = mode === "dev" ? "disksonar-1.2.3-dev-abcdef0" : "disksonar-1.2.3";
    expect(assetPaths(dir, "disksonar", "1.2.3", mode, sha).map(file => basename(file))).toEqual([
      ...["windows-arm64", "windows-x64", "windows-x64-baseline"].map(suffix => prefix + "-" + suffix + ".exe"),
      "SHA256SUMS.txt",
    ]);
  }
});

test("local release set, hashes, PE architecture and bounds", () => {
  verifyAssets(dir, files);
  writeFileSync(files[profiles.length], "bad");
  expect(() => verifyAssets(dir, files)).toThrow("checksum");
  copyFileSync(files[2], files[1]);
  writeFileSync(files[profiles.length], checksums(files));
  expect(() => verifyAssets(dir, files)).toThrow("compiled CPU profile");
  const binary = readFileSync(files[0]);
  binary.writeUInt32LE(0xffffffff, 60);
  writeFileSync(files[0], binary);
  expect(() => verifyAssets(dir, files)).toThrow("PE header");
  writeFileSync(join(dir, "unexpected"), "");
  expect(() => verifyAssets(dir, files)).toThrow("set mismatch");
});

function fixture(prerelease = true) {
  const releaseTag = prerelease ? tag : "v1.2.3";
  const asset = (file: string) => ({
    name: basename(file), size: statSync(file).size, state: "uploaded", digest: "sha256:" + digest(file),
  });
  const release: Release = { id: 42, tag_name: releaseTag, name: releaseTag, target_commitish: sha,
    draft: true, prerelease, assets: files.map(asset) };
  const commands: string[][] = [];
  const state = { emptyLists: 0, createRace: false, missingGets: 0, corruptDownload: false, corruptAfterUpload: false,
    rewritePublishedTag: false, duplicateRelease: false };
  const run = (args: string[]) => {
    commands.push(args);
    if (args.includes("--slurp")) {
      if (state.emptyLists-- > 0) return "[[]]";
      return JSON.stringify([[], state.duplicateRelease ? [release, release] : [release]]);
    }
    if (args.includes("POST")) {
      if (state.createRace) throw new CommandError("gh", 1, "422");
      return JSON.stringify(release);
    }
    if (args.includes("PATCH")) {
      expect(args).toContain("tag_name=" + releaseTag);
      expect(args).toContain("target_commitish=" + sha);
      expect(args).toContain("name=" + releaseTag);
      expect(args).toContain("prerelease=" + prerelease);
      release.draft = args.includes("draft=true");
      release.prerelease = prerelease;
      if (!release.draft && state.rewritePublishedTag) release.tag_name = "untagged-8297";
    } else if (args[1] === "api" && state.missingGets-- > 0) {
      throw new CommandError("gh", 1, "404");
    } else if (args[2] === "delete-asset") {
      release.assets = release.assets.filter(asset => asset.name !== args.at(-1));
    } else if (args[2] === "upload" && !state.corruptAfterUpload) {
      for (const file of args.slice(4, args.indexOf("--clobber"))) {
        release.assets = release.assets.filter(item => item.name !== basename(file));
        release.assets.push(asset(file));
      }
    } else if (args[2] === "download") {
      const filename = args[args.indexOf("--pattern") + 1];
      const content = readFileSync(join(dir, filename));
      if (state.corruptDownload) content[0] ^= 1;
      writeFileSync(join(args[args.indexOf("--dir") + 1], filename), content);
    }
    return JSON.stringify(release);
  };
  const publish = () => publishRelease("owner/repo", releaseTag, sha, prerelease, "notes", files, run, () => {});
  return { release, commands, state, publish };
}

test("create a verified dev or stable draft and publish using its returned ID", () => {
  for (const prerelease of [true, false]) {
    const f = fixture(prerelease);
    f.state.emptyLists = 4;
    f.state.missingGets = 1;
    f.publish();
    const create = f.commands.find(args => args.includes("POST"))!;
    expect(create).toContain("draft=true");
    expect(create).toContain("prerelease=" + prerelease);
    expect(f.release.draft).toBe(false);
    expect(f.commands.filter(args => args.includes("--slurp"))).toHaveLength(4);
  }
});

test("recover from delayed listing, creation race and temporary 404", () => {
  for (const empty of [1, 4]) {
    const f = fixture();
    f.state.emptyLists = empty;
    f.state.createRace = true;
    f.state.missingGets = 2;
    f.publish();
    expect(f.release.draft).toBe(false);
  }
  const f = fixture();
  f.state.duplicateRelease = true;
  expect(f.publish).toThrow("multiple");
});

test("repair draft mode, delete obsolete and replace only missing or invalid assets", () => {
  const f = fixture();
  f.release.prerelease = false;
  f.release.assets[1].size--;
  f.release.assets.pop();
  f.release.assets.push({ name: "obsolete", size: 1, state: "uploaded" });
  f.publish();
  expect(f.commands.filter(args => args.includes("PATCH"))).toHaveLength(2);
  expect(f.commands.find(args => args.includes("PATCH"))).toContain("draft=true");
  expect(f.commands.find(args => args.includes("delete-asset"))).toContain("obsolete");
  expect(f.commands.find(args => args.includes("upload"))!.slice(4, 6)).toEqual([files[1], files[profiles.length]]);
});

test("same-size corruption and absent digest require content verification", () => {
  for (const fallback of [false, true]) {
    for (const corrupt of [false, true]) {
      const f = fixture();
      if (fallback) delete f.release.assets[0].digest;
      else if (corrupt) f.release.assets[0].digest = "sha256:" + "0".repeat(64);
      f.state.corruptDownload = corrupt;
      f.publish();
      expect(f.commands.some(args => args.includes("download"))).toBe(fallback);
      expect(f.commands.some(args => args.includes("upload"))).toBe(corrupt);
    }
  }
});

test("published replays verify identity and asset bytes without mutation", () => {
  const f = fixture();
  f.release.draft = false;
  f.publish();
  f.release.assets[1].digest = "sha256:" + "0".repeat(64);
  expect(f.publish).toThrow("content");
  f.release.prerelease = false;
  expect(f.publish).toThrow("identity");
  expect(f.commands.every(args => args[1] === "api" && !args.includes("--method"))).toBe(true);
});

test("refuse duplicate assets, incomplete upload, rewritten tag and stale identity", () => {
  let f = fixture();
  f.release.assets.push(f.release.assets[0]);
  expect(f.publish).toThrow("duplicate");
  f = fixture();
  f.release.assets.pop();
  f.state.corruptAfterUpload = true;
  expect(f.publish).toThrow("content");
  expect(f.release.draft).toBe(true);
  f = fixture();
  f.state.rewritePublishedTag = true;
  expect(f.publish).toThrow("identity");
  f = fixture();
  f.release.target_commitish = "other";
  expect(f.publish).toThrow("identity");
  expect(f.commands.some(args => args.includes("PATCH"))).toBe(false);
});
