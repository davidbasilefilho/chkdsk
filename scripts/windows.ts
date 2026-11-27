import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

export const root = resolve(import.meta.dir, "..");
export const targets = {
  x64: "x86_64-pc-windows-msvc",
  arm64: "aarch64-pc-windows-msvc",
} as const;
export type Architecture = keyof typeof targets;
export const architectures = Object.keys(targets) as Architecture[];

export class CommandError extends Error {
  constructor(readonly command: string, readonly status: number | null, readonly stderr: string) {
    super(`${command} exited with status ${status}: ${stderr.trim()}`);
  }
}

export function run(args: string[], capture = false, env = process.env, verbatim = false): string {
  console.log("+", ...args);
  const result = spawnSync(args[0], args.slice(1), {
    cwd: root, env, encoding: "utf8", stdio: capture ? "pipe" : "inherit",
    maxBuffer: 64 * 1024 * 1024, windowsVerbatimArguments: verbatim,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new CommandError(args[0], result.status, result.stderr ?? "");
  return result.stdout ?? "";
}

export function linkerFor(arch: Architecture, tools: string): string {
  return join(tools, "bin", "Hostx64", arch, "link.exe");
}

// Cargo may compile host build scripts while compiling C or C++ for ARM64.
// Select each tool by target triple so cc-rs never infers the target from PATH.
export function nativeTools(arch: Architecture, tools: string) {
  const dir = dirname(linkerFor(arch, tools));
  const triple = targets[arch].replaceAll("-", "_");
  return {
    ["CC_" + triple]: join(dir, "cl.exe"),
    ["CXX_" + triple]: join(dir, "cl.exe"),
    ["AR_" + triple]: join(dir, "lib.exe"),
    ["CARGO_TARGET_" + triple.toUpperCase() + "_LINKER"]: join(dir, "link.exe"),
  };
}

export function msvc(arch: Architecture) {
  if (process.platform !== "win32") throw Error("MSVC target tasks must run on Windows");
  const vswhere = join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)",
    "Microsoft Visual Studio", "Installer", "vswhere.exe");
  const install = run([vswhere, "-latest", "-products", "*", "-requires",
    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
    "-property", "installationPath"], true).trim();
  if (!install) throw Error("Visual Studio C++ x86/x64 and ARM64 components are required");
  const batch = join(install, "VC", "Auxiliary", "Build", "vcvarsall.bat");
  if (!existsSync(batch)) throw Error("MSVC environment script not found: " + batch);
  // cmd /c needs its raw command tail, including the quotes around the batch path.
  const captured = run(["cmd.exe", "/d", "/c", `call "${batch}" ${arch === "x64" ? "x64" : "x64_arm64"} >nul && set`],
    true, process.env, true);
  const env: NodeJS.ProcessEnv = {};
  // Windows keys are case-insensitive. Keep only one spelling of PATH.
  for (const line of captured.split(/\r?\n/)) {
    const i = line.indexOf("=");
    if (i > 0) env[line.slice(0, i).toUpperCase()] = line.slice(i + 1);
  }
  if (!env.VCTOOLSINSTALLDIR) throw Error("vcvarsall did not set VCToolsInstallDir");
  const linker = linkerFor(arch, env.VCTOOLSINSTALLDIR);
  const host = linkerFor("x64", env.VCTOOLSINSTALLDIR);
  for (const file of [host, linker]) if (!existsSync(file)) throw Error("MSVC linker not found: " + file);
  env.PATH = [dirname(host), dirname(linker), env.PATH].filter(Boolean).join(";");
  Object.assign(env, nativeTools("x64", env.VCTOOLSINSTALLDIR), nativeTools(arch, env.VCTOOLSINSTALLDIR));
  return { env, linker };
}

export function cargo(arch: Architecture, args: string[]) {
  const { env, linker } = msvc(arch);
  console.log(`MSVC ${arch} linker: ${linker}`);
  run(["cargo", ...args], false, env);
}

export function build(arch: Architecture, name: string) {
  cargo(arch, ["build", "--release", "--target", targets[arch], "--bin", name]);
  const exe = join(root, "target", targets[arch], "release", name + ".exe");
  if (!existsSync(exe)) throw Error("expected release executable was not produced: " + exe);
  return exe;
}
