#!/usr/bin/env python3
"""MSVC-aware validation and draft-first Windows release packaging."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x64": "x86_64-pc-windows-msvc",
    "arm64": "aarch64-pc-windows-msvc",
    "x86": "i686-pc-windows-msvc",
}
# vcvarsall takes host_target; each target uses the matching Hostx64 linker.
MSVC_ENV = {
    "x64": ("x64", "x64"),
    "arm64": ("x64_arm64", "arm64"),
    "x86": ("x64_x86", "x86"),
}
SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
VSWHERE = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
VS_COMPONENTS = (
    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
)


def version_parts(version: str) -> tuple[bool, bool]:
    match = SEMVER.fullmatch(version)
    if not match:
        raise ValueError(f"Cargo package version is not valid semantic versioning: {version!r}")
    return match.group(4) is not None, match.group(5) is not None


def expected_tag(mode: str, version: str, sha: str) -> str:
    prerelease, has_build_metadata = version_parts(version)
    if mode == "stable":
        if prerelease:
            raise ValueError("stable mode requires a normal (non-prerelease) Cargo version")
        return f"v{version}"
    if mode != "dev":
        raise ValueError(f"unsupported release mode: {mode}")
    if not re.fullmatch(r"[0-9a-fA-F]{7,64}", sha):
        raise ValueError("dev mode requires a hexadecimal commit SHA of at least seven characters")
    if has_build_metadata:
        raise ValueError("dev release versions must not contain SemVer build metadata")
    return f"v{version}-dev-{sha[:7].lower()}"


def validate_tag(mode: str, version: str, sha: str, supplied_tag: str | None) -> str:
    tag = expected_tag(mode, version, sha)
    if supplied_tag is not None and supplied_tag != tag:
        raise ValueError(f"tag mismatch: expected {tag!r} for {mode} mode, got {supplied_tag!r}")
    return tag


def manifest() -> tuple[str, str]:
    with (ROOT / "Cargo.toml").open("rb") as stream:
        package = tomllib.load(stream)["package"]
    return package["name"], package["version"]


def target_linker(architecture: str, tools_dir: Path) -> Path:
    _, linker_arch = MSVC_ENV[architecture]
    return tools_dir / "bin" / "Hostx64" / linker_arch / "link.exe"


def msvc_environment(architecture: str) -> tuple[dict[str, str], Path]:
    if os.name != "nt":
        raise RuntimeError("MSVC target tasks must run on Windows")
    if architecture not in TARGETS:
        raise ValueError(f"unknown architecture: {architecture}")
    if not VSWHERE.is_file():
        raise FileNotFoundError(f"Visual Studio locator not found: {VSWHERE}")

    install = subprocess.run(
        [str(VSWHERE), "-latest", "-products", "*", "-requires", *VS_COMPONENTS, "-property", "installationPath"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if not install:
        raise RuntimeError("Visual Studio C++ x86/x64 and ARM64 components are required")
    install_path = Path(install)
    vcvarsall = install_path / "VC" / "Auxiliary" / "Build" / "vcvarsall.bat"
    if not vcvarsall.is_file():
        raise FileNotFoundError(f"MSVC environment script not found: {vcvarsall}")

    # Capture the exact compiler/SDK environment selected by vcvarsall. Prefix
    # explicit host and target linker directories so Cargo never picks a linker
    # for a different architecture merely because it appeared earlier on PATH.
    vcvars_arch, _ = MSVC_ENV[architecture]
    batch = f'call "{vcvarsall}" {vcvars_arch} >nul && set'
    # Pass cmd.exe's command tail as a raw command line. When the batch script
    # is an argument in a list, Python's Windows quoting wraps the entire
    # command for /c; cmd then misparses the embedded quoted path.
    captured = subprocess.run(
        f'cmd.exe /d /c {batch}',
        check=True,
        capture_output=True,
        text=True,
        errors="replace",
    ).stdout
    environment = os.environ.copy()
    for line in captured.splitlines():
        key, separator, value = line.partition("=")
        if separator and key:
            environment[key] = value

    tools_dir_value = environment.get("VCToolsInstallDir")
    if not tools_dir_value:
        raise RuntimeError("vcvarsall did not set VCToolsInstallDir")
    tools_dir = Path(tools_dir_value)
    linker = target_linker(architecture, tools_dir)
    host_linker = tools_dir / "bin" / "Hostx64" / "x64" / "link.exe"
    if not linker.is_file():
        raise FileNotFoundError(f"{architecture} MSVC linker not found: {linker}")
    if not host_linker.is_file():
        raise FileNotFoundError(f"x64 host MSVC linker not found: {host_linker}")

    path_entries = [str(host_linker.parent), str(linker.parent), environment.get("PATH", "")]
    environment["PATH"] = os.pathsep.join(entry for entry in path_entries if entry)
    host_target = TARGETS["x64"].upper().replace("-", "_")
    environment[f"CARGO_TARGET_{host_target}_LINKER"] = str(host_linker)
    target_key = TARGETS[architecture].upper().replace("-", "_")
    environment[f"CARGO_TARGET_{target_key}_LINKER"] = str(linker)
    return environment, linker


def run(command: list[str], *, env: dict[str, str] | None = None, capture: bool = False) -> subprocess.CompletedProcess[str]:
    print("+", subprocess.list2cmdline(command), flush=True)
    return subprocess.run(command, cwd=ROOT, env=env, check=True, text=True, capture_output=capture)


def cargo_for(architecture: str, arguments: list[str]) -> None:
    environment, linker = msvc_environment(architecture)
    print(f"MSVC {architecture} linker: {linker}", flush=True)
    run(["cargo", *arguments], env=environment)


def check_linkers() -> None:
    for architecture in TARGETS:
        _, linker = msvc_environment(architecture)
        print(f"{architecture}: {linker}")


def clippy_all() -> None:
    for architecture, target in TARGETS.items():
        cargo_for(architecture, ["clippy", "--all-targets", "--all-features", "--target", target, "--", "-D", "warnings"])


def compile_all() -> None:
    for architecture, target in TARGETS.items():
        cargo_for(architecture, ["check", "--all-targets", "--all-features", "--target", target])


def build_target(architecture: str, name: str) -> Path:
    target = TARGETS[architecture]
    cargo_for(architecture, ["build", "--release", "--target", target, "--bin", name])
    executable = ROOT / "target" / target / "release" / f"{name}.exe"
    if not executable.is_file():
        raise FileNotFoundError(f"expected release executable was not produced: {executable}")
    return executable


def release_asset_names(name: str, version: str, mode: str, sha: str) -> list[str]:
    suffix = f"-dev-{sha[:7].lower()}" if mode == "dev" else ""
    base = f"{name}-{version}{suffix}-windows-"
    return [base + "x64-baseline.exe", base + "arm64.exe", base + "x86.exe"]


def expected_assets(mode: str, sha: str, name: str, version: str, output: Path) -> list[Path]:
    return [*(output / filename for filename in release_asset_names(name, version, mode, sha)), output / "SHA256SUMS.txt"]


def write_checksums(assets: list[Path], checksum_path: Path) -> None:
    with checksum_path.open("w", encoding="utf-8", newline="\n") as stream:
        for asset in assets:
            stream.write(f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n")


def verify_assets(assets_dir: Path, expected: list[Path]) -> None:
    required = {path.name for path in expected}
    actual = {path.name for path in assets_dir.iterdir() if path.is_file()}
    if actual != required:
        raise ValueError(f"release asset set mismatch: expected {sorted(required)}, found {sorted(actual)}")
    binaries = expected[:-1]
    for binary in binaries:
        if binary.stat().st_size == 0:
            raise ValueError(f"release asset is empty: {binary.name}")
    expected_lines = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}" for path in binaries]
    found_lines = (assets_dir / "SHA256SUMS.txt").read_text(encoding="utf-8").splitlines()
    if found_lines != expected_lines:
        raise ValueError("release checksum manifest does not match the three executable assets")


def package(mode: str, sha: str, tag: str | None, output: Path) -> None:
    name, version = manifest()
    release_tag = validate_tag(mode, version, sha, tag)
    output.mkdir(parents=True, exist_ok=True)
    assets = expected_assets(mode, sha, name, version, output)
    binaries = assets[:-1]
    for architecture, asset in zip(TARGETS, binaries, strict=True):
        shutil.copy2(build_target(architecture, name), asset)
    write_checksums(binaries, assets[-1])
    verify_assets(output, assets)
    print(f"verified release assets for {release_tag}: {', '.join(path.name for path in assets)}")


def gh_json(arguments: list[str]) -> dict:
    result = run(["gh", "api", *arguments], capture=True)
    return json.loads(result.stdout)


def release_id_for_tag(repository: str, tag: str) -> str | None:
    result = run(
        ["gh", "api", "--paginate", "--slurp", f"repos/{repository}/releases"],
        capture=True,
    )
    pages = json.loads(result.stdout)
    matches = [release for page in pages for release in page if release.get("tag_name") == tag]
    if len(matches) > 1:
        raise RuntimeError(f"multiple GitHub releases found for tag {tag}")
    return str(matches[0]["id"]) if matches else None


def verify_remote_assets(draft: dict, assets: list[Path]) -> None:
    if not draft.get("draft"):
        raise RuntimeError("new GitHub release was not retained as a draft")
    remote_assets = draft.get("assets", [])
    remote = {asset.get("name"): asset for asset in remote_assets}
    required = {path.name: path.stat().st_size for path in assets}
    if set(remote) != set(required):
        raise RuntimeError(f"draft asset names mismatch: expected {sorted(required)}, found {sorted(remote)}")
    for filename, size in required.items():
        asset = remote[filename]
        if asset.get("state") != "uploaded" or asset.get("size") != size:
            raise RuntimeError(f"draft asset is incomplete: {filename}")


def resume_remote_assets(repository: str, tag: str, draft: dict, assets: list[Path]) -> None:
    if not draft.get("draft"):
        raise RuntimeError("release is not a draft")
    remote_assets = draft.get("assets", [])
    remote = {asset.get("name"): asset for asset in remote_assets}
    if len(remote) != len(remote_assets):
        raise RuntimeError("draft contains duplicate asset names")
    required = {path.name: path for path in assets}

    for filename in remote.keys() - required.keys():
        run(["gh", "release", "delete-asset", tag, filename, "--yes", "--repo", repository])

    to_upload = [
        path for filename, path in required.items()
        if filename not in remote
        or remote[filename].get("state") != "uploaded"
        or remote[filename].get("size") != path.stat().st_size
    ]
    if to_upload:
        run(["gh", "release", "upload", tag, *(str(path) for path in to_upload),
             "--clobber", "--repo", repository])


def publish(mode: str, sha: str, supplied_tag: str | None, assets_dir: Path) -> None:
    name, version = manifest()
    tag = validate_tag(mode, version, sha, supplied_tag)
    assets = expected_assets(mode, sha, name, version, assets_dir)
    verify_assets(assets_dir, assets)
    repository = os.environ.get("GITHUB_REPOSITORY")
    if not repository:
        raise ValueError("GITHUB_REPOSITORY is required to publish")
    notes = f"Development build for commit {sha[:7]}." if mode == "dev" else f"Stable chkdsk release {version}."

    # Reuse an existing draft on retries: uploading the same names again is
    # not idempotent. The by-tag REST endpoint omits drafts, so resolve the
    # authenticated release list to a database ID and fetch it by ID instead.
    release_id = release_id_for_tag(repository, tag)
    if release_id is None:
        # Create a draft before attaching anything. It becomes public only
        # after the release API confirms the complete, uploaded asset set.
        run(["gh", "release", "create", tag, "--draft", "--title", tag,
             "--notes", notes, "--target", sha])
        release_id = release_id_for_tag(repository, tag)
        if release_id is None:
            raise RuntimeError(f"GitHub did not return the newly created draft for tag {tag}")

    draft = gh_json([f"repos/{repository}/releases/{release_id}"])
    resume_remote_assets(repository, tag, draft, assets)
    draft = gh_json([f"repos/{repository}/releases/{release_id}"])
    verify_remote_assets(draft, assets)
    run(["gh", "api", "--method", "PATCH", f"repos/{repository}/releases/{release_id}", "-F", "draft=false"])
    published = gh_json([f"repos/{repository}/releases/{release_id}"])
    if published.get("draft"):
        raise RuntimeError("GitHub did not publish the verified release draft")
    print(f"published verified release {tag}")


def self_check() -> None:
    assert tuple(TARGETS) == ("x64", "arm64", "x86")
    assert TARGETS == {
        "x64": "x86_64-pc-windows-msvc",
        "arm64": "aarch64-pc-windows-msvc",
        "x86": "i686-pc-windows-msvc",
    }
    assert MSVC_ENV == {
        "x64": ("x64", "x64"),
        "arm64": ("x64_arm64", "arm64"),
        "x86": ("x64_x86", "x86"),
    }
    assert {
        architecture: target_linker(architecture, Path("VC"))
        for architecture in TARGETS
    } == {
        "x64": Path("VC/bin/Hostx64/x64/link.exe"),
        "arm64": Path("VC/bin/Hostx64/arm64/link.exe"),
        "x86": Path("VC/bin/Hostx64/x86/link.exe"),
    }
    assert len(set(TARGETS.values())) == len(TARGETS)
    sha = "abcdef0123456789"
    assert expected_tag("dev", "1.2.3", sha) == "v1.2.3-dev-abcdef0"
    assert expected_tag("dev", "1.2.3-rc.2", sha) == "v1.2.3-rc.2-dev-abcdef0"
    assert expected_tag("stable", "1.2.3", sha) == "v1.2.3"
    assert release_asset_names("chkdsk", "1.2.3", "dev", sha) == [
        "chkdsk-1.2.3-dev-abcdef0-windows-x64-baseline.exe",
        "chkdsk-1.2.3-dev-abcdef0-windows-arm64.exe",
        "chkdsk-1.2.3-dev-abcdef0-windows-x86.exe",
    ]
    assert release_asset_names("chkdsk", "1.2.3", "stable", sha) == [
        "chkdsk-1.2.3-windows-x64-baseline.exe",
        "chkdsk-1.2.3-windows-arm64.exe",
        "chkdsk-1.2.3-windows-x86.exe",
    ]
    for invalid in ("01.2.3", "1.2", "1.2.3-01"):
        try:
            version_parts(invalid)
        except ValueError:
            continue
        raise AssertionError(f"invalid SemVer accepted: {invalid}")
    try:
        expected_tag("stable", "1.2.3-rc.1", sha)
    except ValueError:
        pass
    else:
        raise AssertionError("stable mode accepted a prerelease Cargo version")
    try:
        validate_tag("stable", "1.2.3", sha, "v1.2.4")
    except ValueError:
        pass
    else:
        raise AssertionError("stable mode accepted a mismatched tag")

    global run
    original_run = run
    mock_commands: list[list[str]] = []
    try:
        def mock_run(command: list[str], *, env: dict[str, str] | None = None, capture: bool = False) -> subprocess.CompletedProcess[str]:
            mock_commands.append(command)
            response = json.dumps([[
                {"id": 41, "tag_name": "v1.2.3"},
                {"id": 42, "tag_name": "v1.2.3-dev-abcdef0"},
            ]]) if len(mock_commands) == 1 else json.dumps([[]])
            return subprocess.CompletedProcess(command, 0, stdout=response, stderr="")

        run = mock_run
        assert release_id_for_tag("owner/repo", "v1.2.3-dev-abcdef0") == "42"
        assert release_id_for_tag("owner/repo", "missing") is None
        assert mock_commands == [
            ["gh", "api", "--paginate", "--slurp", "repos/owner/repo/releases"],
            ["gh", "api", "--paginate", "--slurp", "repos/owner/repo/releases"],
        ]
    finally:
        run = original_run

    with tempfile.TemporaryDirectory() as directory:
        asset_paths = [Path(directory) / name for name in ("one.exe", "two.exe", "SHA256SUMS.txt")]
        for path, content in zip(asset_paths, (b"one", b"two", b"checksums"), strict=True):
            path.write_bytes(content)
        remote = {
            "draft": True,
            "assets": [
                {"name": path.name, "size": path.stat().st_size, "state": "uploaded"}
                for path in asset_paths
            ],
        }
        verify_remote_assets(remote, asset_paths)
        mock_commands.clear()
        try:
            run = lambda command, *, env=None, capture=False: (
                mock_commands.append(command) or subprocess.CompletedProcess(command, 0, stdout="", stderr="")
            )
            resume_remote_assets("owner/repo", "v1.2.3", remote, asset_paths)
            assert not mock_commands, "complete assets should not be re-uploaded"

            partial = {
                "draft": True,
                "assets": [
                    remote["assets"][0],
                    {**remote["assets"][1], "size": 1},
                    {"name": "obsolete.txt", "size": 1, "state": "uploaded"},
                ],
            }
            resume_remote_assets("owner/repo", "v1.2.3", partial, asset_paths)
            assert mock_commands == [
                ["gh", "release", "delete-asset", "v1.2.3", "obsolete.txt", "--yes", "--repo", "owner/repo"],
                ["gh", "release", "upload", "v1.2.3", str(asset_paths[1]), str(asset_paths[2]),
                 "--clobber", "--repo", "owner/repo"],
            ], f"unexpected draft resume commands: {mock_commands}"
        finally:
            run = original_run
        try:
            verify_remote_assets({**remote, "assets": remote["assets"][:-1]}, asset_paths)
        except RuntimeError:
            pass
        else:
            raise AssertionError("incomplete remote draft asset set was accepted")
        print("release self-check passed: SemVer, asset names, target/linker mapping, paginated draft lookup, and asset resume/verification")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("self-check", help="check release names, assets, and target/linker mapping")
    subparsers.add_parser("linker-check", help="verify architecture-specific MSVC environments and linkers")
    clippy = subparsers.add_parser("clippy", help="lint all targets and features for every MSVC target")
    tests = subparsers.add_parser("tests", help="run the x64 Windows test suite")
    compile_parser = subparsers.add_parser("compile", help="compile all Cargo targets for every MSVC target")
    build = subparsers.add_parser("build", help="build one architecture's optimized executable")
    package_parser = subparsers.add_parser("package", help="build and package the complete Windows release asset set")
    publish_parser = subparsers.add_parser("publish", help="create draft, verify assets, then publish")
    cargo = subparsers.add_parser("cargo", help="run one Cargo command in an architecture-specific MSVC environment")

    build.add_argument("--architecture", choices=TARGETS, required=True)
    package_parser.add_argument("--mode", choices=("dev", "stable"), required=True)
    package_parser.add_argument("--sha", required=True)
    package_parser.add_argument("--tag")
    package_parser.add_argument("--output", type=Path, default=Path("dist/release"))
    publish_parser.add_argument("--mode", choices=("dev", "stable"), required=True)
    publish_parser.add_argument("--sha", required=True)
    publish_parser.add_argument("--tag")
    publish_parser.add_argument("--assets-dir", type=Path, default=Path("dist/release"))
    cargo.add_argument("--architecture", choices=TARGETS, required=True)
    cargo.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    try:
        if args.command == "self-check":
            self_check()
        elif args.command == "linker-check":
            check_linkers()
        elif args.command == "clippy":
            clippy_all()
        elif args.command == "tests":
            cargo_for("x64", ["test", "--all-targets", "--all-features", "--target", TARGETS["x64"]])
        elif args.command == "compile":
            compile_all()
        elif args.command == "build":
            name, _ = manifest()
            output = build_target(args.architecture, name)
            print(f"built {output}")
        elif args.command == "package":
            package(args.mode, args.sha, args.tag, (ROOT / args.output).resolve())
        elif args.command == "publish":
            publish(args.mode, args.sha, args.tag, (ROOT / args.assets_dir).resolve())
        else:
            arguments = args.arguments
            if arguments[:1] == ["--"]:
                arguments = arguments[1:]
            if not arguments:
                raise ValueError("cargo command is required after --")
            cargo_for(args.architecture, arguments)
    except (OSError, KeyError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"release error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
