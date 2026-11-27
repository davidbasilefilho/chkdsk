import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { assetPaths, manifest, packageAssets, tagFor, verifyAssets } from "./assets";
import { publishRelease } from "./github";
import { architectures, profiles, type Profile, build, cargo, msvc, root, targets, type Architecture } from "./windows";

const commands = ["self-check", "linker-check", "clippy", "tests", "compile", "build", "package", "publish", "cargo"];
const usage = "usage: bun scripts/release.ts {" + commands.join(",") + "}";
class UsageError extends Error {}

function main([command, ...argv]: string[]) {
  const architectureOption = ["build", "cargo"].includes(command);
  const releaseOption = ["package", "publish"].includes(command);
  const directoryOption = command === "package" ? "output" : "assets-dir";
  const options = architectureOption ? ["architecture"] : releaseOption ? ["mode", "sha", "tag", directoryOption] : [];
  let rest: string[] = [];
  if (command === "cargo") {
    const end = argv[0] === "--architecture" ? 2 : argv[0]?.startsWith("--architecture=") ? 1 : 0;
    rest = argv.slice(end);
    if (rest[0] === "--") rest.shift();
    argv = argv.slice(0, end);
  }
  if (["-h", "--help"].includes(command) || argv.some(arg => ["-h", "--help"].includes(arg))) {
    console.log(usage, ...options.map(name => "--" + name));
    return;
  }
  if (!commands.includes(command)) throw new UsageError(usage);
  let args;
  try {
    args = parseArgs({ args: argv, options: Object.fromEntries(options.map(name => [name, { type: "string" }])) }).values;
  } catch (error) { throw new UsageError(String(error)); }
  const arch = args.architecture as Architecture;
  if (command === "build" && !profiles.includes(args.architecture as Profile)) throw new UsageError("--architecture requires arm64, x64 or x64-baseline");
  if (command === "cargo" && !architectures.includes(arch)) throw new UsageError("--architecture requires x64 or arm64");
  switch (command) {
    case "self-check": {
      const result = Bun.spawnSync([process.execPath, "test", "./scripts/release.test.ts"], { cwd: root, stdout: "inherit", stderr: "inherit" });
      if (result.exitCode !== 0) throw Error("release self-check failed");
      console.log("release self-check passed: lookup recovery, draft identity/prerelease repair, asset content, and publication verification");
      return;
    }
    case "linker-check":
      for (const arch of architectures) console.log(arch + ": " + msvc(arch).linker);
      return;
    case "tests": return cargo("x64", ["test", "--all-targets", "--all-features", "--target", targets.x64]);
    case "compile":
    case "clippy":
      for (const arch of architectures) {
        const lint = command === "clippy";
        cargo(arch, [lint ? "clippy" : "check", "--all-targets", "--all-features", "--target", targets[arch],
          ...(lint ? ["--", "-D", "warnings"] : [])]);
      }
      return;
    case "cargo":
      if (!rest.length) throw Error("cargo command is required after --");
      return cargo(arch, rest);
    case "build": return console.log("built", build(args.architecture as Profile, manifest().name));
  }
  const { mode, sha } = args;
  if (mode !== "stable" && mode !== "dev") throw new UsageError("--mode requires dev or stable");
  if (typeof sha !== "string") throw new UsageError("--sha is required");
  const { name, version } = manifest();
  const tag = tagFor(mode, version, sha, args.tag as string | undefined);
  const dir = resolve(root, (args[directoryOption] as string | undefined) ?? "dist/release");
  const files = assetPaths(dir, name, version, mode, sha);
  if (command === "package") return packageAssets(dir, files, name, tag);
  verifyAssets(dir, files);
  const repo = process.env.GITHUB_REPOSITORY;
  if (!repo) throw Error("GITHUB_REPOSITORY is required to publish");
  const dev = mode === "dev";
  publishRelease(repo, tag, sha, dev, dev ? `Development build for commit ${sha.slice(0, 7)}.` : `Stable chkdsk release ${version}.`, files);
}

try { main(process.argv.slice(2)); }
catch (error) {
  console.error("release error:", error instanceof Error ? error.message : error);
  process.exitCode = error instanceof UsageError ? 2 : 1;
}
