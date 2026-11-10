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
import time
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x64": "x86_64-pc-windows-msvc",
    "arm64": "aarch64-pc-windows-msvc",
}
# vcvarsall takes host_target; each target uses the matching Hostx64 linker.
MSVC_ENV = {
    "x64": ("x64", "x64"),
    "arm64": ("x64_arm64", "arm64"),
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
    return [base + "x64-baseline.exe", base + "arm64.exe"]


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
        raise ValueError("release checksum manifest does not match the two executable assets")


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


def verify_executables(binaries: list[Path], name: str, version: str) -> None:
    if len(binaries) != len(TARGETS):
        raise ValueError("exactly two Windows executables are required")
    for architecture, binary in zip(TARGETS, binaries, strict=True):
        verify_pe_machine(binary, architecture)
    if os.name != "nt":
        raise RuntimeError("packaged x64 version smoke checks require Windows")
    # --version returns before elevation or the window start; never open the GUI here.
    for architecture, binary in zip(TARGETS, binaries, strict=True):
        if architecture == "arm64":
            continue  # PE validation only; the Windows x64 runner cannot execute ARM64.
        result = subprocess.run([str(binary), "--version"], check=True, capture_output=True, text=True, timeout=30)
        expected = f"{name} {version}"
        if result.stdout.strip() != expected:
            raise ValueError(f"unexpected --version from {binary.name}: {result.stdout!r}; expected {expected!r}")
        print(f"verified {architecture} --version: {result.stdout.strip()}", flush=True)


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
    verify_executables(binaries, name, version)
    print(f"verified release assets for {release_tag}: {', '.join(path.name for path in assets)}")


def gh_json(arguments: list[str]) -> dict:
    result = run(["gh", "api", *arguments], capture=True)
    return json.loads(result.stdout)


def release_id_for_tag(repository: str, tag: str, *, retries: int = 3) -> str | None:
    for attempt in range(retries + 1):
        result = run(
            ["gh", "api", "--paginate", "--slurp", f"repos/{repository}/releases"],
            capture=True,
        )
        pages = json.loads(result.stdout)
        matches = [release for page in pages for release in page if release.get("tag_name") == tag]
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


def publish(mode: str, sha: str, supplied_tag: str | None, assets_dir: Path) -> None:
    name, version = manifest()
    tag = validate_tag(mode, version, sha, supplied_tag)
    assets = expected_assets(mode, sha, name, version, assets_dir)
    verify_assets(assets_dir, assets)
    verify_executables(assets[:-1], name, version)
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


def self_check() -> None:
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
    assert expected_tag("dev", "1.2.3", sha) == "v1.2.3-dev-abcdef0"
    assert expected_tag("dev", "1.2.3-rc.2", sha) == "v1.2.3-rc.2-dev-abcdef0"
    assert expected_tag("stable", "1.2.3", sha) == "v1.2.3"
    assert release_asset_names("chkdsk", "1.2.3", "dev", sha) == [
        "chkdsk-1.2.3-dev-abcdef0-windows-x64-baseline.exe",
        "chkdsk-1.2.3-dev-abcdef0-windows-arm64.exe",
    ]
    assert release_asset_names("chkdsk", "1.2.3", "stable", sha) == [
        "chkdsk-1.2.3-windows-x64-baseline.exe",
        "chkdsk-1.2.3-windows-arm64.exe",
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
        binaries = [Path(directory) / f"test-{architecture}.exe" for architecture in TARGETS]
        original_name, original_subprocess_run = os.name, subprocess.run
        calls = []
        original_verify_pe_machine = globals()["verify_pe_machine"]
        try:
            os.name = "nt"
            globals()["verify_pe_machine"] = lambda binary, architecture: None

            def smoke_run(command, **kwargs):
                calls.append(command)
                assert kwargs.get("check") and kwargs.get("timeout") == 30
                return subprocess.CompletedProcess(command, 0, stdout="chkdsk 1.2.3\n", stderr="")

            subprocess.run = smoke_run
            verify_executables(binaries, "chkdsk", "1.2.3")
            assert calls == [[str(binaries[0]), "--version"]]
            try:
                verify_executables(binaries, "chkdsk", "9.9.9")
            except ValueError:
                pass
            else:
                raise AssertionError("incorrect executable version was accepted")
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
        print("release self-check passed: lookup recovery, draft identity/prerelease repair, asset content, and publication verification")


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
