#!/usr/bin/env python3
"""MSVC-aware validation, release planning and draft-first Windows release packaging."""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path
from typing import NamedTuple
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
# 32-bit x86 is deliberately absent: rune does not build for i686-pc-windows-msvc.
TARGETS = {
    "x64": "x86_64-pc-windows-msvc",
    "arm64": "aarch64-pc-windows-msvc",
}
# vcvarsall takes host_target; each target uses the matching Hostx64 linker.
MSVC_ENV = {
    "x64": ("x64", "x64"),
    "arm64": ("x64_arm64", "arm64"),
}


class Variant(NamedTuple):
    architecture: str
    cpu: str | None  # rustc -C target-cpu; None keeps the target default


# Release assets, in publication order. The key is the asset suffix.
#   x64          x86-64-v3 (AVX2): Haswell/Excavator or newer.
#   x64-baseline x86-64-v2 (SSE4.2, POPCNT): the Windows 11 24H2 minimum CPU.
#   arm64        the default armv8-a baseline; no arm64-baseline variant exists.
# The names were checked against `rustc --print target-cpus --target x86_64-pc-windows-msvc`.
VARIANTS = {
    "x64": Variant("x64", "x86-64-v3"),
    "x64-baseline": Variant("x64", "x86-64-v2"),
    "arm64": Variant("arm64", None),
}
SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
# The project is ZeroVer: every release is 0.y.z, without prerelease or build metadata.
ZEROVER = re.compile(r"0\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
STABLE_TAG = re.compile(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
# v<version>-dev.<YYYY-MM-DD>.<HH-MM-SS>.g<sha7>. Both date parts are fixed-width
# SemVer identifiers, so ASCII comparison orders tags chronologically.
DEV_TAG = re.compile(
    r"v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)-dev\.\d{4}-\d{2}-\d{2}\.\d{2}-\d{2}-\d{2}\.g[0-9a-f]{7}"
)
# The earlier scheme, v<version>-dev-<sha7>; such releases are still pruned.
LEGACY_DEV_TAG = re.compile(r"v\d+\.\d+\.\d+-dev-[0-9a-f]{7,40}")
DOCS_ONLY_PATTERNS = ("README.md", "*.md", "docs/*", "LICENSE")
KEEP_DEV_RELEASES = 10
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


def require_zerover(version: str) -> tuple[int, int, int]:
    version_parts(version)
    if not ZEROVER.fullmatch(version):
        raise ValueError(f"version must be ZeroVer 0.y.z without prerelease or build metadata: {version!r}")
    major, minor, patch = (int(part) for part in version.split("."))
    return major, minor, patch


def dev_suffix(sha: str, committed: int) -> str:
    """`dev.<date>.<time>.g<sha7>` from the commit time in UTC; one commit, one suffix."""
    when = datetime.fromtimestamp(committed, tz=timezone.utc)
    if not 1970 <= when.year <= 9999:
        raise ValueError(f"commit time is outside the supported range: {committed}")
    return f"dev.{when:%Y-%m-%d}.{when:%H-%M-%S}.g{sha[:7].lower()}"


def expected_tag(mode: str, version: str, sha: str, committed: int | None = None) -> str:
    require_zerover(version)
    if mode == "stable":
        return f"v{version}"
    if mode != "dev":
        raise ValueError(f"unsupported release mode: {mode}")
    if not re.fullmatch(r"[0-9a-fA-F]{7,64}", sha):
        raise ValueError("dev mode requires a hexadecimal commit SHA of at least seven characters")
    if committed is None:
        raise ValueError("dev mode requires the commit time")
    return f"v{version}-{dev_suffix(sha, committed)}"


def validate_tag(mode: str, version: str, sha: str, committed: int | None, supplied_tag: str | None) -> str:
    tag = expected_tag(mode, version, sha, committed)
    if supplied_tag is not None and supplied_tag != tag:
        raise ValueError(f"tag mismatch: expected {tag!r} for {mode} mode, got {supplied_tag!r}")
    return tag


def is_dev_tag(tag: str) -> bool:
    return bool(DEV_TAG.fullmatch(tag) or LEGACY_DEV_TAG.fullmatch(tag))


def release_inputs(
    sha: str | None, tag: str | None, head: str, environ: dict[str, str] | None = None
) -> tuple[str, str | None]:
    """Release commit and optional tag from flags, else RELEASE_SHA/RELEASE_TAG.

    The workflow never interpolates ref names into a shell line. The commit
    defaults to the checked-out HEAD and must equal it: a pull_request event's
    GITHUB_SHA is a merge commit, and building the wrong commit would publish
    assets under another commit's tag.
    """
    environment = os.environ if environ is None else environ
    sha = (sha or environment.get("RELEASE_SHA") or head).lower()
    if sha != head.lower():
        raise ValueError(f"the checked-out commit {head} is not the release commit {sha}")
    return sha, tag or environment.get("RELEASE_TAG") or None


def git(*arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout.strip()


def git_head() -> str:
    return git("rev-parse", "HEAD")


def commit_time(sha: str) -> int:
    return int(git("show", "-s", "--format=%ct", sha))


BINARY_MANIFEST = Path("crates") / "chkdsk" / "Cargo.toml"


def manifest(root: Path = ROOT) -> tuple[str, str]:
    """Binary name from the binary crate, version from `[workspace.package]`."""
    with (root / "Cargo.toml").open("rb") as stream:
        version = tomllib.load(stream)["workspace"]["package"]["version"]
    with (root / BINARY_MANIFEST).open("rb") as stream:
        name = tomllib.load(stream)["package"]["name"]
    return name, version


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
    environment[f"CARGO_TARGET_{target_key(TARGETS['x64'])}_LINKER"] = str(host_linker)
    environment[f"CARGO_TARGET_{target_key(TARGETS[architecture])}_LINKER"] = str(linker)
    return environment, linker


def run(command: list[str], *, env: dict[str, str] | None = None, capture: bool = False) -> subprocess.CompletedProcess[str]:
    print("+", subprocess.list2cmdline(command), flush=True)
    return subprocess.run(command, cwd=ROOT, env=env, check=True, text=True, capture_output=capture)


def target_key(target: str) -> str:
    return target.upper().replace("-", "_")


def variant_environment(variant: str, base: dict[str, str]) -> dict[str, str]:
    """Environment that applies the variant's target-cpu to its target only.

    RUSTFLAGS outranks target-specific flags in Cargo, so it is dropped rather
    than letting an ambient value silently replace the CPU level.
    """
    architecture, cpu = VARIANTS[variant]
    environment = {key: value for key, value in base.items() if key not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")}
    environment[f"CARGO_TARGET_{target_key(TARGETS[architecture])}_RUSTFLAGS"] = f"-C target-cpu={cpu}" if cpu else ""
    return environment


def cargo_for(architecture: str, arguments: list[str], *, variant: str | None = None) -> None:
    environment, linker = msvc_environment(architecture)
    if variant is not None:
        environment = variant_environment(variant, environment)
    print(f"MSVC {architecture} linker: {linker}", flush=True)
    run(["cargo", *arguments], env=environment)


def check_linkers() -> None:
    for architecture in TARGETS:
        _, linker = msvc_environment(architecture)
        print(f"{architecture}: {linker}")


def clippy_all() -> None:
    for architecture, target in TARGETS.items():
        cargo_for(architecture, ["clippy", "--workspace", "--all-targets", "--all-features", "--target", target, "--", "-D", "warnings"])


def compile_all() -> None:
    for architecture, target in TARGETS.items():
        cargo_for(architecture, ["check", "--workspace", "--all-targets", "--all-features", "--target", target])


def xwin_clippy_all() -> None:
    """Cross-lint the Windows targets from Linux with cargo-xwin (no Windows runner)."""
    for target in TARGETS.values():
        run(["cargo", "xwin", "clippy", "--workspace", "--all-targets", "--all-features", "--target", target, "--", "-D", "warnings"])


def build_target(variant: str, name: str) -> Path:
    """Release build into a per-variant target dir, so variants never share artifacts."""
    architecture, _ = VARIANTS[variant]
    target = TARGETS[architecture]
    target_dir = ROOT / "target" / variant
    cargo_for(
        architecture,
        ["build", "--release", "--package", name, "--target", target, "--target-dir", str(target_dir)],
        variant=variant,
    )
    executable = target_dir / target / "release" / f"{name}.exe"
    if not executable.is_file():
        raise FileNotFoundError(f"expected release executable was not produced: {executable}")
    return executable


def release_asset_names(name: str, tag: str) -> list[str]:
    return [f"{name}-{tag.removeprefix('v')}-windows-{variant}.exe" for variant in VARIANTS]


def expected_assets(tag: str, name: str, output: Path) -> list[Path]:
    return [*(output / filename for filename in release_asset_names(name, tag)), output / "SHA256SUMS.txt"]


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
        raise ValueError("release checksum manifest does not match the executable assets")


def verify_pe_machine(binary: Path, architecture: str) -> None:
    expected_machine = {"x64": 0x8664, "arm64": 0xAA64}[architecture]
    data = binary.read_bytes()
    if len(data) < 64 or data[:2] != b"MZ":
        raise ValueError(f"invalid DOS executable header: {binary.name}")
    pe_offset = int.from_bytes(data[0x3C:0x40], "little")
    if pe_offset < 64 or pe_offset + 24 > len(data) or data[pe_offset:pe_offset + 4] != b"PE\0\0":
        raise ValueError(f"invalid PE executable header: {binary.name}")
    machine = int.from_bytes(data[pe_offset + 4:pe_offset + 6], "little")
    if machine != expected_machine:
        raise ValueError(f"wrong PE architecture for {binary.name}: expected {expected_machine:#06x}, got {machine:#06x}")
    print(f"verified PE architecture {architecture}: {binary.name}", flush=True)


def smoke_environment() -> dict[str, str]:
    """Environment for running a freshly built executable, without GitHub credentials."""
    return {key: value for key, value in os.environ.items() if key.upper() not in ("GH_TOKEN", "GITHUB_TOKEN")}


def verify_executable(binary: Path, variant: str, name: str, version: str, *, smoke: bool) -> None:
    architecture, _ = VARIANTS[variant]
    verify_pe_machine(binary, architecture)
    if not smoke or architecture != "x64":
        return  # ARM64 gets PE validation only; the Windows x64 runner cannot execute it.
    if os.name != "nt":
        raise RuntimeError("x64 version smoke checks require Windows")
    # --version returns before elevation or the window start; never open the GUI here.
    result = subprocess.run(
        [str(binary), "--version"],
        check=True, capture_output=True, text=True, timeout=30, env=smoke_environment(),
    )
    expected = f"{name} {version}"
    if result.stdout.strip() != expected:
        raise ValueError(f"unexpected --version from {binary.name}: {result.stdout!r}; expected {expected!r}")
    print(f"verified {variant} --version: {result.stdout.strip()}", flush=True)


def verify_executables(binaries: list[Path], name: str, version: str, *, smoke: bool) -> None:
    if len(binaries) != len(VARIANTS):
        raise ValueError(f"exactly {len(VARIANTS)} Windows executables are required")
    for variant, binary in zip(VARIANTS, binaries, strict=True):
        verify_executable(binary, variant, name, version, smoke=smoke)


def resolve_release(mode: str, sha: str | None, supplied_tag: str | None) -> tuple[str, str, str, str]:
    """(binary name, version, commit, tag) for the checked-out commit."""
    sha, tag = release_inputs(sha, supplied_tag, git_head())
    name, version = manifest()
    committed = commit_time(sha) if mode == "dev" else None
    return name, version, sha, validate_tag(mode, version, sha, committed, tag)


def build(mode: str, variant: str, sha: str | None, tag: str | None, output: Path) -> Path:
    """Build one variant into the release directory under its final asset name."""
    name, version, _, release_tag = resolve_release(mode, sha, tag)
    output.mkdir(parents=True, exist_ok=True)
    asset = output / release_asset_names(name, release_tag)[list(VARIANTS).index(variant)]
    shutil.copy2(build_target(variant, name), asset)
    verify_executable(asset, variant, name, version, smoke=True)
    return asset


def checksums(mode: str, sha: str | None, tag: str | None, assets_dir: Path) -> None:
    """Check the three executables (after any signing), then write SHA256SUMS.txt."""
    name, version, _, release_tag = resolve_release(mode, sha, tag)
    assets = expected_assets(release_tag, name, assets_dir)
    missing = [path.name for path in assets[:-1] if not path.is_file()]
    if missing:
        raise FileNotFoundError(f"release executables are missing: {missing}")
    verify_executables(assets[:-1], name, version, smoke=False)
    write_checksums(assets[:-1], assets[-1])
    verify_assets(assets_dir, assets)
    print(f"checksums written for {release_tag}: {', '.join(path.name for path in assets)}")


def gh_json(arguments: list[str]) -> dict:
    result = run(["gh", "api", *arguments], capture=True)
    return json.loads(result.stdout)


def list_releases(repository: str) -> list[dict]:
    """Every release, drafts included (a tag lookup cannot see drafts)."""
    result = run(["gh", "api", "--paginate", "--slurp", f"repos/{repository}/releases"], capture=True)
    return [release for page in json.loads(result.stdout) for release in page]


def release_id_for_tag(repository: str, tag: str, *, retries: int = 3) -> str | None:
    for attempt in range(retries + 1):
        matches = [release for release in list_releases(repository) if release.get("tag_name") == tag]
        if len(matches) > 1:
            raise RuntimeError(f"multiple GitHub releases found for tag {tag}")
        if matches:
            return str(matches[0]["id"])
        if attempt < retries:
            time.sleep(0.25 * (attempt + 1))
    return None


def create_draft(repository: str, tag: str, sha: str, notes: str, prerelease: bool) -> dict:
    return gh_json([
        "--method", "POST", f"repos/{repository}/releases",
        "-F", f"tag_name={tag}", "-F", f"target_commitish={sha}",
        "-F", f"name={tag}", "-F", f"body={notes}",
        "-F", "draft=true", "-F", f"prerelease={str(prerelease).lower()}",
    ])


def publish_draft(repository: str, release_id: str, tag: str, sha: str, prerelease: bool) -> None:
    run([
        "gh", "api", "--method", "PATCH", f"repos/{repository}/releases/{release_id}",
        "-F", f"tag_name={tag}", "-F", f"name={tag}", "-F", f"target_commitish={sha}",
        "-F", "draft=false", "-F", f"prerelease={str(prerelease).lower()}",
    ], capture=True)


def correct_draft_prerelease(
    repository: str, release_id: str, tag: str, sha: str, draft: dict, prerelease: bool
) -> None:
    if not draft.get("draft"):
        raise RuntimeError("cannot change prerelease status on a published release")
    if draft.get("prerelease") is prerelease:
        return
    run([
        "gh", "api", "--method", "PATCH", f"repos/{repository}/releases/{release_id}",
        "-F", f"tag_name={tag}", "-F", f"name={tag}", "-F", f"target_commitish={sha}",
        "-F", "draft=true", "-F", f"prerelease={str(prerelease).lower()}",
    ], capture=True)


def gh_json_retry(arguments: list[str], *, retries: int = 3) -> dict:
    for attempt in range(retries + 1):
        try:
            return gh_json(arguments)
        except subprocess.CalledProcessError as error:
            if "404" not in str(error.stderr) or attempt == retries:
                raise
            time.sleep(0.25 * (attempt + 1))
    raise AssertionError("unreachable")


def sha256_file(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def remote_asset_matches(repository: str, tag: str, remote_asset: dict, local_path: Path) -> bool:
    local_digest = sha256_file(local_path)
    remote_digest = remote_asset.get("digest")
    if isinstance(remote_digest, str) and re.fullmatch(r"sha256:[0-9a-fA-F]{64}", remote_digest):
        return remote_digest.lower() == f"sha256:{local_digest}"

    with tempfile.TemporaryDirectory() as directory:
        run([
            "gh", "release", "download", tag, "--repo", repository,
            "--pattern", local_path.name, "--dir", directory,
        ])
        downloaded = Path(directory) / local_path.name
        if not downloaded.is_file():
            raise RuntimeError(f"GitHub did not download release asset for verification: {local_path.name}")
        return sha256_file(downloaded) == local_digest


def verify_release_identity(release: dict, tag: str, sha: str, prerelease: bool) -> None:
    expected = {
        "tag_name": tag,
        "name": tag,
        "target_commitish": sha,
        "prerelease": prerelease,
    }
    mismatches = {
        field: (expected_value, release.get(field))
        for field, expected_value in expected.items()
        if release.get(field) != expected_value
    }
    if mismatches:
        raise RuntimeError(f"release identity mismatch (expected, found): {mismatches}")


def verify_release_assets(repository: str, tag: str, release: dict, assets: list[Path]) -> None:
    remote_assets = release.get("assets", [])
    remote = {asset.get("name"): asset for asset in remote_assets}
    if len(remote) != len(remote_assets):
        raise RuntimeError("release contains duplicate asset names")
    required_assets = {path.name: path for path in assets}
    required = {name: path.stat().st_size for name, path in required_assets.items()}
    if set(remote) != set(required):
        raise RuntimeError(f"draft asset names mismatch: expected {sorted(required)}, found {sorted(remote)}")
    for filename, size in required.items():
        asset = remote[filename]
        if asset.get("state") != "uploaded" or asset.get("size") != size:
            raise RuntimeError(f"release asset is incomplete: {filename}")
        if not remote_asset_matches(repository, tag, asset, required_assets[filename]):
            raise RuntimeError(f"release asset content does not match the local asset: {filename}")


def verify_remote_assets(
    repository: str, tag: str, sha: str, draft: dict, assets: list[Path], prerelease: bool
) -> None:
    if not draft.get("draft"):
        raise RuntimeError("new GitHub release was not retained as a draft")
    verify_release_identity(draft, tag, sha, prerelease)
    verify_release_assets(repository, tag, draft, assets)


def verify_published_release(release: dict, tag: str, sha: str, prerelease: bool) -> None:
    if release.get("draft"):
        raise RuntimeError("GitHub did not publish the verified release draft")
    verify_release_identity(release, tag, sha, prerelease)


def verify_existing_published_release(
    repository: str, tag: str, sha: str, release: dict, assets: list[Path], prerelease: bool
) -> bool:
    if release.get("draft"):
        return False
    verify_published_release(release, tag, sha, prerelease)
    verify_release_assets(repository, tag, release, assets)
    return True


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
        or not remote_asset_matches(repository, tag, remote[filename], path)
    ]
    if to_upload:
        run(["gh", "release", "upload", tag, *(str(path) for path in to_upload),
             "--clobber", "--repo", repository])


def publish(mode: str, sha: str | None, supplied_tag: str | None, assets_dir: Path) -> None:
    name, version, sha, tag = resolve_release(mode, sha, supplied_tag)
    assets = expected_assets(tag, name, assets_dir)
    verify_assets(assets_dir, assets)
    # The x64 smoke test ran on Windows in the build job; the Linux publisher
    # re-checks the PE headers of the (possibly signed) files it uploads.
    verify_executables(assets[:-1], name, version, smoke=False)
    repository = os.environ.get("GITHUB_REPOSITORY")
    if not repository:
        raise ValueError("GITHUB_REPOSITORY is required to publish")
    notes = f"Development build for commit {sha[:7]}." if mode == "dev" else f"Stable chkdsk release {version}."

    prerelease = mode == "dev"
    # Reuse an existing draft on retries. If absent, create it through the API
    # and use the returned ID directly; release-list visibility can lag creation.
    release_id = release_id_for_tag(repository, tag)
    created_draft = None
    if release_id is None:
        try:
            created_draft = create_draft(repository, tag, sha, notes, prerelease)
            release_id = str(created_draft["id"])
        except subprocess.CalledProcessError:
            # A parallel retry may have created the tag after our last list
            # response; make a bounded lookup before surfacing the API failure.
            release_id = release_id_for_tag(repository, tag)
            if release_id is None:
                raise

    draft = created_draft or gh_json_retry([f"repos/{repository}/releases/{release_id}"])
    if verify_existing_published_release(repository, tag, sha, draft, assets, prerelease):
        print(f"verified already-published release {tag}")
        return
    correct_draft_prerelease(repository, release_id, tag, sha, draft, prerelease)
    draft = gh_json_retry([f"repos/{repository}/releases/{release_id}"])
    verify_release_identity(draft, tag, sha, prerelease)
    resume_remote_assets(repository, tag, draft, assets)
    draft = gh_json_retry([f"repos/{repository}/releases/{release_id}"])
    verify_remote_assets(repository, tag, sha, draft, assets, prerelease)
    publish_draft(repository, release_id, tag, sha, prerelease)
    published = gh_json_retry([f"repos/{repository}/releases/{release_id}"])
    verify_published_release(published, tag, sha, prerelease)
    print(f"published verified release {tag}")


def docs_only(paths: list[str]) -> bool:
    """True when every changed path is documentation; no change at all counts as code."""
    return bool(paths) and all(
        any(fnmatch.fnmatchcase(path, pattern) for pattern in DOCS_ONLY_PATTERNS) for path in paths
    )


def stable_versions(tags: list[str]) -> list[tuple[int, int, int]]:
    return [
        (int(parts[0]), int(parts[1]), int(parts[2]))
        for tag in tags
        if STABLE_TAG.fullmatch(tag)
        for parts in [tag[1:].split(".")]
    ]


def check_version_bump(version: str, tags: list[str]) -> None:
    """The version must be ZeroVer and strictly above every stable tag."""
    key = require_zerover(version)
    latest = max(stable_versions(tags), default=None)
    if latest is not None and key <= latest:
        raise ValueError(
            f"version {version} must be greater than the latest stable release "
            f"v{'.'.join(map(str, latest))}: bump [workspace.package] version in Cargo.toml"
        )


def check_pr_source(head_ref: str, head_repo: str, repository: str) -> None:
    if head_repo != repository or head_ref != "dev":
        raise ValueError(f"pull requests into main must come from the dev branch of {repository}, not {head_repo}:{head_ref}")


def decide_dev(*, docs: bool, duplicate: bool, fork: bool, released: bool) -> tuple[bool, bool, str]:
    """(validate, release, reason) for a push to dev or a pull request."""
    if duplicate:
        return False, False, "an open pull request from this branch runs the same commit"
    if docs:
        return False, False, "documentation-only change"
    if fork:
        return True, False, "pull request from a fork gets no pre-release"
    if released:
        return True, False, "a release for this commit already exists"
    return True, True, "new commit"


def decide_stable(
    version: str, sha: str, tag_commit: str | None, release_published: bool, tags: list[str]
) -> tuple[bool, str]:
    """(skip, reason); raises when the release would contradict an existing tag or version."""
    tag = f"v{version}"
    require_zerover(version)
    if tag_commit is None:
        check_version_bump(version, tags)
        return False, "new version"
    if tag_commit != sha:
        raise ValueError(
            f"tag {tag} already exists at {tag_commit[:7]}, not {sha[:7]}: "
            "stable tags are immutable; bump the version instead"
        )
    return release_published, "already released" if release_published else "resuming an incomplete release"


def select_prune(releases: list[dict], keep: int) -> list[dict]:
    """Dev pre-releases beyond the newest `keep` by creation date; stable releases are never listed."""
    dev = [
        release for release in releases
        if release.get("prerelease") and is_dev_tag(str(release.get("tag_name", "")))
    ]
    dev.sort(key=lambda release: (str(release.get("created_at", "")), release.get("id", 0)), reverse=True)
    return dev[keep:]


def prune_dev_releases(repository: str, keep: int) -> None:
    for release in select_prune(list_releases(repository), keep):
        tag = release["tag_name"]
        print(f"pruning dev pre-release {tag}", flush=True)
        # A concurrent run may already have deleted it; that is the desired end state.
        for command in (
            ["gh", "api", "--method", "DELETE", f"repos/{repository}/releases/{release['id']}"],
            ["gh", "api", "--method", "DELETE", f"repos/{repository}/git/refs/tags/{tag}"],
        ):
            try:
                run(command, capture=True)
            except subprocess.CalledProcessError as error:
                print(f"warning: {' '.join(command[4:])} failed: {error.stderr.strip()}", flush=True)


def changed_paths(base: str | None, head: str, *, merge_base: bool) -> list[str] | None:
    """Paths changed since `base`, or None when the base is unknown (treated as code)."""
    if not base or set(base) == {"0"}:
        return None
    try:
        return git("diff", "--name-only", f"{base}...{head}" if merge_base else f"{base}..{head}").splitlines()
    except subprocess.CalledProcessError:
        return None


def write_outputs(values: dict[str, str]) -> None:
    for key, value in values.items():
        print(f"{key}={value}")
    destination = os.environ.get("GITHUB_OUTPUT")
    if destination:
        with open(destination, "a", encoding="utf-8", newline="\n") as stream:
            stream.writelines(f"{key}={value}\n" for key, value in values.items())


def plan_dev() -> None:
    """Decide what a push to dev or a pull request runs; inputs come from the environment."""
    environment = os.environ
    event = environment["EVENT_NAME"]
    repository = environment["GITHUB_REPOSITORY"]
    sha = git_head()
    name, version = manifest()
    tag = expected_tag("dev", version, sha, commit_time(sha))
    paths = changed_paths(environment.get("BASE_SHA"), sha, merge_base=event == "pull_request")
    fork = event == "pull_request" and environment["HEAD_REPO"] != repository
    duplicate = False
    if event == "push":
        # A push to a branch with an open pull request also triggers that pull
        # request's run; let the pull request cover the commit.
        owner = repository.split("/")[0]
        pulls = json.loads(run(
            ["gh", "api", "--method", "GET", f"repos/{repository}/pulls",
             "-f", "state=open", "-f", f"head={owner}:{environment['BRANCH']}"],
            capture=True,
        ).stdout)
        duplicate = bool(pulls)
    docs = paths is not None and docs_only(paths)
    released = False
    if not duplicate and not docs and not fork:
        released = any(
            release.get("tag_name") == tag and not release.get("draft") for release in list_releases(repository)
        )
    validate, release, reason = decide_dev(docs=docs, duplicate=duplicate, fork=fork, released=released)
    print(f"{name} {tag}: {reason}")
    write_outputs({"validate": str(validate).lower(), "release": str(release).lower(), "sha": sha, "tag": tag})


def plan_stable() -> None:
    environment = os.environ
    repository = environment["GITHUB_REPOSITORY"]
    sha = git_head()
    _, version = manifest()
    tag = f"v{version}"
    tags = git("tag", "--list", "v*").splitlines()
    tag_commit = None
    if tag in tags:
        tag_commit = git("rev-parse", f"refs/tags/{tag}^{{commit}}")
    published = tag_commit is not None and any(
        release.get("tag_name") == tag and not release.get("draft") for release in list_releases(repository)
    )
    skip, reason = decide_stable(version, sha, tag_commit, published, tags)
    print(f"{tag}: {reason}")
    write_outputs({"skip": str(skip).lower(), "sha": sha, "tag": tag})


def check_version() -> None:
    """Gate for a pull request into main: dev as the source, a ZeroVer bump as the content."""
    environment = os.environ
    check_pr_source(environment["HEAD_REF"], environment["HEAD_REPO"], environment["GITHUB_REPOSITORY"])
    _, version = manifest()
    check_version_bump(version, git("tag", "--list", "v*").splitlines())
    print(f"version {version} is a valid ZeroVer bump")


def check_variants() -> None:
    assert list(VARIANTS) == ["x64", "x64-baseline", "arm64"], "x86 and arm64-baseline must stay out"
    assert {variant: spec.cpu for variant, spec in VARIANTS.items()} == {
        "x64": "x86-64-v3", "x64-baseline": "x86-64-v2", "arm64": None,
    }
    assert all(spec.architecture in TARGETS for spec in VARIANTS.values())
    ambient = {"PATH": "p", "RUSTFLAGS": "-C target-cpu=native", "CARGO_ENCODED_RUSTFLAGS": "x"}
    assert variant_environment("x64", ambient) == {
        "PATH": "p", "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS": "-C target-cpu=x86-64-v3",
    }, "x64 must get its CPU level on its own target and drop ambient RUSTFLAGS"
    assert variant_environment("x64-baseline", {})["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS"] == "-C target-cpu=x86-64-v2"
    assert variant_environment("arm64", {}) == {"CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_RUSTFLAGS": ""}, "arm64 keeps the default CPU"


def check_dev_tags() -> None:
    sha = "50eb54a8d6a7b950bcca5406f250de6d3fac9eda"
    committed = 1794314702  # 2026-11-10 12:45:02 UTC
    tag = expected_tag("dev", "0.1.0", sha, committed)
    assert tag == "v0.1.0-dev.2026-11-10.12-45-02.g50eb54a"
    assert tag == expected_tag("dev", "0.1.0", sha.upper(), committed), "the same commit must yield the same tag"
    assert DEV_TAG.fullmatch(tag) and is_dev_tag(tag) and is_dev_tag("v0.0.2-dev-0663029")
    assert validate_tag("dev", "0.1.0", sha, committed, tag) == tag
    for invalid in (
        lambda: validate_tag("dev", "0.1.0", sha, committed, "v0.1.0-dev-50eb54a"),
        lambda: expected_tag("dev", "0.1.0", sha),
        lambda: expected_tag("dev", "0.1.0", "50eb5", committed),
        lambda: expected_tag("dev", "0.1.0+build", sha, committed),
        lambda: dev_suffix(sha, 10**12),
    ):
        try:
            invalid()
        except ValueError:
            continue
        raise AssertionError("an invalid dev tag input was accepted")
    assert not any(is_dev_tag(stable) for stable in ("v0.1.0", "v0.1.0-rc.1", "v0.1.0-dev", "v0.1.0-dev.x"))

    def precedence(tag: str) -> tuple:
        """SemVer 2.0 precedence: numeric identifiers below alphanumeric ones, ASCII order."""
        core, _, prerelease = tag.removeprefix("v").partition("-")
        numbers = tuple(int(part) for part in core.split("."))
        if not prerelease:
            return numbers, (1,)
        identifiers = tuple((0, int(part), "") if part.isdigit() else (1, 0, part) for part in prerelease.split("."))
        return numbers, (0, identifiers)

    times = [
        datetime(2025, 12, 31, 23, 59, 59), datetime(2026, 1, 1, 0, 0, 0), datetime(2026, 1, 1, 0, 0, 1),
        datetime(2026, 2, 28, 23, 59, 59), datetime(2026, 9, 9, 9, 9, 9), datetime(2026, 9, 30, 23, 0, 0),
        datetime(2026, 10, 1, 0, 0, 0), datetime(2026, 11, 9, 23, 59, 59), datetime(2026, 11, 10, 9, 5, 7),
        datetime(2026, 11, 10, 10, 0, 0), datetime(2026, 11, 10, 12, 45, 2), datetime(2027, 1, 2, 3, 4, 5),
    ]
    epochs = [int(moment.replace(tzinfo=timezone.utc).timestamp()) for moment in times]
    # Hash prefixes run opposite to time, so only the date and time can explain the order.
    tags = [
        expected_tag("dev", "0.1.0", f"{255 - index:02x}" * 4, epoch) for index, epoch in enumerate(epochs)
    ]
    assert all(DEV_TAG.fullmatch(tag) for tag in tags)
    shuffled = sorted(tags, key=lambda tag: tag[::-1])
    assert sorted(shuffled, key=precedence) == tags, "semver precedence order must equal chronological order"
    assert sorted(tags) == tags, "plain ASCII order must be chronological too"
    # Releases of a version order between its dev builds and the next version.
    ordered = [*tags[:3], "v0.1.0", "v0.1.1-dev.2026-01-01.00-00-00.gaaaaaaa", "v0.1.1", "v0.2.0"]
    assert sorted(reversed(ordered), key=precedence) == ordered
    assert precedence("v0.1.0-dev.2026-11-10.12-45-02.gabcdef0") < precedence("v0.1.0-dev.2026-11-10.12-45-02.gabcdef1")
    assert sorted([*tags, "v0.1.0-dev-abcdef0"], key=precedence)  # legacy tags still sort without error


def check_zerover() -> None:
    assert require_zerover("0.1.0") == (0, 1, 0)
    for invalid in ("1.0.0", "0.1", "0.01.0", "0.1.0-rc.1", "0.1.0+build", "00.1.0"):
        try:
            require_zerover(invalid)
        except ValueError:
            continue
        raise AssertionError(f"non-ZeroVer version accepted: {invalid}")
    tags = ["v0.0.1", "v0.1.0-dev-abcdef0", "v0.1.0-dev.2026-11-10.12-45-02.g50eb54a", "untagged-8297", "v0.0.10", "v1.0.0-rc.1"]
    assert stable_versions(tags) == [(0, 0, 1), (0, 0, 10)]
    check_version_bump("0.0.11", tags)
    check_version_bump("0.1.0", [])
    for version in ("0.0.10", "0.0.9", "0.0.1", "1.0.0", "0.0.11-rc.1"):
        try:
            check_version_bump(version, tags)
        except ValueError:
            continue
        raise AssertionError(f"version without a valid bump accepted: {version}")
    check_pr_source("dev", "owner/repo", "owner/repo")
    for head_ref, head_repo in (("feature", "owner/repo"), ("dev", "fork/repo"), ("main", "owner/repo")):
        try:
            check_pr_source(head_ref, head_repo, "owner/repo")
        except ValueError:
            continue
        raise AssertionError(f"pull request into main from {head_repo}:{head_ref} accepted")


def check_plans() -> None:
    assert docs_only(["README.md"]) and docs_only(["docs/a/b.png", "LICENSE", "crates/x/NOTES.md"])
    assert not docs_only([]), "an unknown or empty diff must run the full validation"
    assert not docs_only(["README.md", "crates/chkdsk/src/main.rs"])
    assert not docs_only([".github/workflows/dev.yml"]) and not docs_only(["scripts/release.py"])
    assert not docs_only(["Cargo.toml"]) and not docs_only(["LICENSE.txt"])
    assert changed_paths(None, "HEAD", merge_base=True) is None
    assert changed_paths("0" * 40, "HEAD", merge_base=False) is None
    assert decide_dev(docs=False, duplicate=False, fork=False, released=False) == (True, True, "new commit")
    assert decide_dev(docs=False, duplicate=False, fork=False, released=True)[:2] == (True, False), "a released commit is built once"
    assert decide_dev(docs=False, duplicate=False, fork=True, released=False)[:2] == (True, False), "forks get no pre-release"
    assert decide_dev(docs=True, duplicate=False, fork=False, released=False)[:2] == (False, False)
    assert decide_dev(docs=False, duplicate=True, fork=False, released=False)[:2] == (False, False)
    sha, other = "a" * 40, "b" * 40
    tags = ["v0.0.1", "v0.1.0"]
    assert decide_stable("0.2.0", sha, None, False, tags)[0] is False
    assert decide_stable("0.1.0", sha, sha, True, tags)[0] is True, "a published release is not repeated"
    assert decide_stable("0.1.0", sha, sha, False, tags)[0] is False, "an incomplete release resumes"
    for version, tag_commit in (("0.1.0", other), ("0.0.5", None), ("1.0.0", None), ("0.1.0", None)):
        try:
            decide_stable(version, sha, tag_commit, False, tags)
        except ValueError:
            continue
        raise AssertionError(f"stable release of {version} (tag at {tag_commit}) was not refused")


def check_prune() -> None:
    def release(tag: str, created: str, number: int, *, prerelease: bool = True) -> dict:
        return {"id": number, "tag_name": tag, "created_at": created, "prerelease": prerelease, "draft": False}

    dev = [
        release(f"v0.1.0-dev.2026-11-{day:02d}.10-00-00.g{day:07x}", f"2026-11-{day:02d}T10:00:00Z", day)
        for day in range(1, 13)
    ]
    legacy = [release("v0.0.1-dev-14c9d73", "2026-10-05T10:00:00Z", 100), release("v0.1.0-dev-449e7e2", "2026-10-06T10:00:00Z", 101)]
    stable = [release("v0.0.1", "2026-10-01T00:00:00Z", 200, prerelease=False), release("v0.2.0", "2020-01-01T00:00:00Z", 201, prerelease=False)]
    lookalike = release("v0.3.0-dev-abcdef0", "2019-01-01T00:00:00Z", 202, prerelease=False)
    other = release("untagged-8297a20833e1b75c0756", "2019-01-01T00:00:00Z", 203)
    everything = [*stable, lookalike, other, *reversed(dev), *legacy]
    pruned = select_prune(everything, KEEP_DEV_RELEASES)
    assert [item["id"] for item in pruned] == [2, 1, 101, 100], "the oldest dev pre-releases go, legacy ones included"
    assert not {200, 201, 202, 203} & {item["id"] for item in pruned}, "stable and unrelated releases are never pruned"
    assert select_prune(everything, 0)[0]["id"] == 12 and len(select_prune(everything, 0)) == 14
    assert select_prune(everything, 14) == [] and select_prune([], 10) == []
    same_time = [release("v0.1.0-dev-1111111", "2026-10-01T00:00:00Z", 1), release("v0.1.0-dev-2222222", "2026-10-01T00:00:00Z", 2)]
    assert [item["id"] for item in select_prune(same_time, 1)] == [1], "equal creation dates fall back to the newer id"

    global run
    original_run = run
    commands: list[list[str]] = []

    def fake_run(command, *, env=None, capture=False):
        commands.append(command)
        if command[:3] == ["gh", "api", "--paginate"]:
            return subprocess.CompletedProcess(command, 0, stdout=json.dumps([everything[:20], everything[20:]]), stderr="")
        if command[-1].endswith("/git/refs/tags/v0.1.0-dev.2026-11-02.10-00-00.g0000002"):
            raise subprocess.CalledProcessError(422, command, stderr="Reference does not exist")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    try:
        run = fake_run
        prune_dev_releases("owner/repo", KEEP_DEV_RELEASES)
    finally:
        run = original_run
    deletions = [command[-1] for command in commands[1:]]
    assert len(deletions) == 8 and deletions[:2] == [
        "repos/owner/repo/releases/2", "repos/owner/repo/git/refs/tags/v0.1.0-dev.2026-11-02.10-00-00.g0000002",
    ]
    assert all("/releases/20" not in path for path in deletions)


def self_check() -> None:
    with tempfile.TemporaryDirectory() as directory:
        fixture = Path(directory)
        (fixture / BINARY_MANIFEST).parent.mkdir(parents=True)
        (fixture / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/*"]\n\n[workspace.package]\nversion = "1.2.3"\n')
        (fixture / BINARY_MANIFEST).write_text('[package]\nname = "chkdsk"\nversion.workspace = true\n')
        assert manifest(fixture) == ("chkdsk", "1.2.3")
    assert manifest()[0] == "chkdsk", "the executable must stay chkdsk.exe"
    version_parts(manifest()[1])
    assert tuple(TARGETS) == ("x64", "arm64")
    assert TARGETS == {
        "x64": "x86_64-pc-windows-msvc",
        "arm64": "aarch64-pc-windows-msvc",
    }
    assert MSVC_ENV == {
        "x64": ("x64", "x64"),
        "arm64": ("x64_arm64", "arm64"),
    }
    assert {
        architecture: target_linker(architecture, Path("VC"))
        for architecture in TARGETS
    } == {
        "x64": Path("VC/bin/Hostx64/x64/link.exe"),
        "arm64": Path("VC/bin/Hostx64/arm64/link.exe"),
    }
    assert len(set(TARGETS.values())) == len(TARGETS)
    sha = "abcdef0123456789"
    committed = 1778417102  # 2026-05-10 12:45:02 UTC
    assert expected_tag("dev", "0.2.3", sha, committed) == "v0.2.3-dev.2026-05-10.12-45-02.gabcdef0"
    assert expected_tag("stable", "0.2.3", sha) == "v0.2.3"
    assert release_inputs(None, None, sha, {}) == (sha, None)
    assert release_inputs(sha[:7].upper() + sha[7:], "v0.2.3", sha, {"RELEASE_SHA": "x"}) == (sha, "v0.2.3")
    assert release_inputs(None, None, sha, {"RELEASE_SHA": sha, "RELEASE_TAG": "v0.2.3"}) == (sha, "v0.2.3")
    try:
        release_inputs("1234567", None, sha, {})
    except ValueError:
        pass
    else:
        raise AssertionError("a release commit other than the checked-out HEAD was accepted")
    assert release_asset_names("chkdsk", "v0.2.3-dev.2026-05-10.12-45-02.gabcdef0") == [
        "chkdsk-0.2.3-dev.2026-05-10.12-45-02.gabcdef0-windows-x64.exe",
        "chkdsk-0.2.3-dev.2026-05-10.12-45-02.gabcdef0-windows-x64-baseline.exe",
        "chkdsk-0.2.3-dev.2026-05-10.12-45-02.gabcdef0-windows-arm64.exe",
    ]
    assert release_asset_names("chkdsk", "v0.2.3") == [
        "chkdsk-0.2.3-windows-x64.exe",
        "chkdsk-0.2.3-windows-x64-baseline.exe",
        "chkdsk-0.2.3-windows-arm64.exe",
    ]
    assert [path.name for path in expected_assets("v0.2.3", "chkdsk", Path("out"))][-1] == "SHA256SUMS.txt"
    check_variants()
    check_dev_tags()
    check_zerover()
    check_plans()
    check_prune()
    for invalid in ("01.2.3", "1.2", "1.2.3-01"):
        try:
            version_parts(invalid)
        except ValueError:
            continue
        raise AssertionError(f"invalid SemVer accepted: {invalid}")
    try:
        validate_tag("stable", "0.2.3", sha, None, "v0.2.4")
    except ValueError:
        pass
    else:
        raise AssertionError("stable mode accepted a mismatched tag")

    with tempfile.TemporaryDirectory() as directory:
        binary = Path(directory) / "test.exe"
        for architecture, machine in (("x64", 0x8664), ("arm64", 0xAA64)):
            data = bytearray(88)
            data[:2] = b"MZ"
            data[0x3C:0x40] = (64).to_bytes(4, "little")
            data[64:68] = b"PE\0\0"
            data[68:70] = machine.to_bytes(2, "little")
            binary.write_bytes(data)
            verify_pe_machine(binary, architecture)
            wrong = "arm64" if architecture == "x64" else "x64"
            try:
                verify_pe_machine(binary, wrong)
            except ValueError:
                pass
            else:
                raise AssertionError("incorrect PE machine was accepted")
        for invalid in (b"", b"MZ", b"MZ" + bytes(86)):
            binary.write_bytes(invalid)
            try:
                verify_pe_machine(binary, "x64")
            except ValueError:
                pass
            else:
                raise AssertionError("invalid PE header was accepted")

    with tempfile.TemporaryDirectory() as directory:
        binaries = [Path(directory) / f"test-{variant}.exe" for variant in VARIANTS]
        original_name, original_subprocess_run = os.name, subprocess.run
        calls = []
        original_verify_pe_machine = globals()["verify_pe_machine"]
        try:
            os.name = "nt"
            globals()["verify_pe_machine"] = lambda binary, architecture: None

            def smoke_run(command, **kwargs):
                calls.append(command)
                assert kwargs.get("check") and kwargs.get("timeout") == 30
                smoke_env = kwargs.get("env")
                assert smoke_env is not None and not {"GH_TOKEN", "GITHUB_TOKEN"} & {key.upper() for key in smoke_env}
                return subprocess.CompletedProcess(command, 0, stdout="chkdsk 1.2.3\n", stderr="")

            subprocess.run = smoke_run
            with mock.patch.dict(os.environ, {"GH_TOKEN": "secret", "GITHUB_TOKEN": "secret", "KEEP": "1"}):
                verify_executables(binaries, "chkdsk", "1.2.3", smoke=True)
                assert smoke_environment().get("KEEP") == "1"
            assert calls == [[str(binaries[0]), "--version"], [str(binaries[1]), "--version"]], "both x64 variants are smoke-tested, ARM64 is not"
            calls.clear()
            verify_executables(binaries, "chkdsk", "1.2.3", smoke=False)
            assert not calls, "a publisher without Windows must not execute the binaries"
            try:
                verify_executables(binaries, "chkdsk", "9.9.9", smoke=True)
            except ValueError:
                pass
            else:
                raise AssertionError("incorrect executable version was accepted")
            try:
                verify_executables(binaries[:2], "chkdsk", "1.2.3", smoke=False)
            except ValueError:
                pass
            else:
                raise AssertionError("an incomplete executable set was accepted")
        finally:
            os.name = original_name
            subprocess.run = original_subprocess_run
            globals()["verify_pe_machine"] = original_verify_pe_machine

    global run
    original_run = run
    mock_commands: list[list[str]] = []
    mock_capture_flags: list[bool] = []
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
        assert release_id_for_tag("owner/repo", "missing", retries=0) is None
        assert mock_commands == [
            ["gh", "api", "--paginate", "--slurp", "repos/owner/repo/releases"],
            ["gh", "api", "--paginate", "--slurp", "repos/owner/repo/releases"],
        ]

        mock_commands.clear()
        responses = iter((json.dumps([[]]), json.dumps([[{"id": 43, "tag_name": "v1.2.3-dev-abcdef0"}]])))
        run = lambda command, *, env=None, capture=False: (
            mock_commands.append(command)
            or subprocess.CompletedProcess(command, 0, stdout=next(responses), stderr="")
        )
        original_sleep = time.sleep
        time.sleep = lambda _: None
        try:
            assert release_id_for_tag("owner/repo", "v1.2.3-dev-abcdef0", retries=1) == "43"
            assert len(mock_commands) == 2, "release lookup did not retry once after an empty page"
        finally:
            time.sleep = original_sleep

        mock_commands.clear()

        def mock_create(command: list[str], *, env: dict[str, str] | None = None, capture: bool = False) -> subprocess.CompletedProcess[str]:
            mock_commands.append(command)
            mock_capture_flags.append(capture)
            return subprocess.CompletedProcess(command, 0, stdout='{"id":99}', stderr="")

        run = mock_create
        assert create_draft("owner/repo", "v1.2.3-dev-abcdef0", sha, "notes", True)["id"] == 99
        assert "-F" in mock_commands[0] and "prerelease=true" in mock_commands[0]
        assert create_draft("owner/repo", "v1.2.3", sha, "notes", False)["id"] == 99
        assert "prerelease=false" in mock_commands[1]
        mock_commands.clear()
        mock_capture_flags.clear()
        dev_tag = "v1.2.3-dev-abcdef0"
        correct_draft_prerelease(
            "owner/repo", "99", dev_tag, sha,
            {"draft": True, "prerelease": False}, True,
        )
        assert mock_commands == [[
            "gh", "api", "--method", "PATCH", "repos/owner/repo/releases/99",
            "-F", f"tag_name={dev_tag}", "-F", f"name={dev_tag}", "-F", f"target_commitish={sha}",
            "-F", "draft=true", "-F", "prerelease=true",
        ]], "existing draft mode was not corrected while retaining draft status"
        assert mock_capture_flags == [True], "draft PATCH response was not captured"
        mock_commands.clear()
        mock_capture_flags.clear()
        correct_draft_prerelease(
            "owner/repo", "99", dev_tag, sha, {"draft": True, "prerelease": True}, True
        )
        assert not mock_commands, "matching draft mode should not be patched"
        try:
            correct_draft_prerelease(
                "owner/repo", "99", dev_tag, sha, {"draft": False, "prerelease": False}, True
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("published release was allowed through draft mode correction")
        publish_draft("owner/repo", "99", dev_tag, sha, True)
        publish_draft("owner/repo", "100", "v1.2.3", sha, False)
        assert "prerelease=true" in mock_commands[0] and "draft=false" in mock_commands[0]
        assert f"tag_name={dev_tag}" in mock_commands[0] and f"name={dev_tag}" in mock_commands[0]
        assert f"target_commitish={sha}" in mock_commands[0]
        assert "prerelease=false" in mock_commands[1] and "draft=false" in mock_commands[1]
        assert "tag_name=v1.2.3" in mock_commands[1] and "name=v1.2.3" in mock_commands[1]
        assert f"target_commitish={sha}" in mock_commands[1]
        assert mock_capture_flags == [True, True], "publication PATCH responses were not captured"
    finally:
        run = original_run

    with tempfile.TemporaryDirectory() as directory:
        asset_paths = [Path(directory) / name for name in ("one.exe", "two.exe", "SHA256SUMS.txt")]
        for path, content in zip(asset_paths, (b"one", b"two", b"checksums"), strict=True):
            path.write_bytes(content)
        remote = {
            "draft": True,
            "prerelease": True,
            "tag_name": "v1.2.3-dev-abcdef0",
            "name": "v1.2.3-dev-abcdef0",
            "target_commitish": sha,
            "assets": [
                {
                    "name": path.name,
                    "size": path.stat().st_size,
                    "state": "uploaded",
                    "digest": f"sha256:{sha256_file(path)}",
                }
                for path in asset_paths
            ],
        }
        verify_remote_assets("owner/repo", "v1.2.3-dev-abcdef0", sha, remote, asset_paths, True)
        verify_remote_assets(
            "owner/repo", "v1.2.3", sha,
            {**remote, "prerelease": False, "tag_name": "v1.2.3", "name": "v1.2.3"},
            asset_paths, False,
        )
        verify_published_release({**remote, "draft": False}, "v1.2.3-dev-abcdef0", sha, True)
        verify_published_release(
            {**remote, "draft": False, "prerelease": False, "tag_name": "v1.2.3", "name": "v1.2.3"},
            "v1.2.3", sha, False,
        )
        mock_commands.clear()
        already_published = {**remote, "draft": False}
        assert verify_existing_published_release(
            "owner/repo", "v1.2.3-dev-abcdef0", sha, already_published, asset_paths, True
        ), "a matching published release was not reused"
        assert not mock_commands, "verifying an already-published release mutated or downloaded anything"
        try:
            verify_existing_published_release(
                "owner/repo", "v1.2.3-dev-abcdef0", sha,
                {
                    **already_published,
                    "assets": [
                        remote["assets"][0],
                        {**remote["assets"][1], "digest": "sha256:" + "0" * 64},
                        remote["assets"][2],
                    ],
                },
                asset_paths,
                True,
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("published release with corrupted same-size asset was reused")
        assert not mock_commands, "digest-based published-release verification caused a write"
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

            mock_commands.clear()
            corrupted = {
                **remote,
                "assets": [
                    remote["assets"][0],
                    {**remote["assets"][1], "digest": "sha256:" + "0" * 64},
                    remote["assets"][2],
                ],
            }
            resume_remote_assets("owner/repo", "v1.2.3", corrupted, asset_paths)
            assert mock_commands == [[
                "gh", "release", "upload", "v1.2.3", str(asset_paths[1]),
                "--clobber", "--repo", "owner/repo",
            ]], f"same-size corrupted asset was not replaced: {mock_commands}"

            mock_commands.clear()
            fallback_assets = [{**asset} for asset in remote["assets"]]
            fallback_assets[0].pop("digest")
            digest_missing = {**remote, "assets": fallback_assets}
            local_by_name = {path.name: path for path in asset_paths}

            def mock_download(command: list[str], *, env=None, capture=False):
                mock_commands.append(command)
                assert "--pattern" in command, command
                name = command[command.index("--pattern") + 1]
                directory = Path(command[command.index("--dir") + 1])
                (directory / name).write_bytes(local_by_name[name].read_bytes())
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

            run = mock_download
            resume_remote_assets("owner/repo", "v1.2.3", digest_missing, asset_paths)
            assert len(mock_commands) == 1 and mock_commands[0][1:3] == ["release", "download"]

            mock_commands.clear()

            def mock_corrupt_download(command: list[str], *, env=None, capture=False):
                mock_commands.append(command)
                if command[1:3] != ["release", "download"]:
                    return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
                assert "--pattern" in command, command
                name = command[command.index("--pattern") + 1]
                directory = Path(command[command.index("--dir") + 1])
                local = local_by_name[name].read_bytes()
                (directory / name).write_bytes(bytes([local[0] ^ 1]) + local[1:])
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

            run = mock_corrupt_download
            resume_remote_assets("owner/repo", "v1.2.3", digest_missing, asset_paths)
            assert len(mock_commands) == 2 and mock_commands[0][1:3] == ["release", "download"]
            assert "--clobber" in mock_commands[1], "downloaded same-size corruption was not replaced"
        finally:
            run = original_run
        try:
            verify_remote_assets(
                "owner/repo", "v1.2.3-dev-abcdef0", sha,
                {**remote, "assets": remote["assets"][:-1]}, asset_paths, True,
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("incomplete remote draft asset set was accepted")
        try:
            verify_published_release(
                {**remote, "draft": False, "prerelease": False}, "v1.2.3-dev-abcdef0", sha, True
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("published prerelease mismatch was accepted")
        try:
            verify_published_release(
                {**remote, "draft": False, "tag_name": "untagged-8297"},
                "v1.2.3-dev-abcdef0", sha, True,
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("published release with a rewritten tag was accepted")
        print("release self-check passed: dev tags and ordering, ZeroVer bump, planning, pruning, variants, lookup recovery, draft repair, asset content, and publication verification")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("self-check", help="check release naming, planning, pruning and target/linker mapping")
    subparsers.add_parser("linker-check", help="verify architecture-specific MSVC environments and linkers")
    subparsers.add_parser("clippy", help="lint all targets and features for every MSVC target")
    subparsers.add_parser("xwin-clippy", help="lint every Windows target from Linux with cargo-xwin")
    subparsers.add_parser("tests", help="run the x64 Windows test suite")
    subparsers.add_parser("compile", help="compile all Cargo targets for every MSVC target")
    subparsers.add_parser("plan-dev", help="decide what a push to dev or a pull request validates and releases")
    subparsers.add_parser("plan-stable", help="decide whether a push to main releases, and refuse conflicting tags")
    subparsers.add_parser("check-version", help="verify a pull request into main comes from dev with a ZeroVer bump")
    prune_parser = subparsers.add_parser("prune", help="delete dev pre-releases and tags beyond the newest N")
    build_parser = subparsers.add_parser("build", help="build one variant's optimized executable as a release asset")
    checksums_parser = subparsers.add_parser("checksums", help="verify the three executables and write SHA256SUMS.txt")
    publish_parser = subparsers.add_parser("publish", help="create draft, verify assets, then publish")
    cargo = subparsers.add_parser("cargo", help="run one Cargo command in an architecture-specific MSVC environment")

    prune_parser.add_argument("--keep", type=int, default=KEEP_DEV_RELEASES)
    build_parser.add_argument("--variant", choices=VARIANTS, required=True)
    build_parser.add_argument("--output", type=Path, default=Path("dist/release"))
    for command in (build_parser, checksums_parser, publish_parser):
        command.add_argument("--mode", choices=("dev", "stable"), required=True)
        command.add_argument("--sha", help="release commit; defaults to RELEASE_SHA, else the checked-out HEAD")
        command.add_argument("--tag", help="expected tag; defaults to RELEASE_TAG, else it is derived")
    checksums_parser.add_argument("--assets-dir", type=Path, default=Path("dist/release"))
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
        elif args.command == "xwin-clippy":
            xwin_clippy_all()
        elif args.command == "tests":
            cargo_for("x64", ["test", "--workspace", "--all-targets", "--all-features", "--target", TARGETS["x64"]])
        elif args.command == "compile":
            compile_all()
        elif args.command == "plan-dev":
            plan_dev()
        elif args.command == "plan-stable":
            plan_stable()
        elif args.command == "check-version":
            check_version()
        elif args.command == "prune":
            prune_dev_releases(os.environ["GITHUB_REPOSITORY"], args.keep)
        elif args.command == "build":
            print(f"built {build(args.mode, args.variant, args.sha, args.tag, ROOT / args.output)}")
        elif args.command == "checksums":
            checksums(args.mode, args.sha, args.tag, (ROOT / args.assets_dir).resolve())
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
