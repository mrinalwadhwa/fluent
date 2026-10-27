#!/usr/bin/env python3
"""Prepare a pinned reader from a verified, project-owned executable cache."""

import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid


def digest(data):
    return hashlib.sha256(data).hexdigest()


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd)


def write_proof(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as staged:
        staged.write(content)
    Path(staged.name).replace(path)


def checked_config_digest(path):
    content = path.read_bytes()
    # Cargo may load files named by include, including paths outside this tree.
    # Reject even possible escaped keys until those files can be verified.
    if b"include" in content or b"\\u" in content or b"\\U" in content:
        raise ValueError(f"Cargo configuration include cannot be verified: {path}")
    return digest(content)


def cargo_cwd_config(build_cwd):
    config = {}
    for directory in (build_cwd, *build_cwd.parents):
        for name in ("config.toml", "config"):
            path = directory / ".cargo" / name
            if path.is_file():
                config[str(path)] = checked_config_digest(path)
    return config


def verified_entry(entry, identity):
    binary = entry / "fluent"
    provenance = entry / "provenance.json"
    if not (entry.is_dir() and not entry.is_symlink() and binary.is_file()
            and not binary.is_symlink() and provenance.is_file() and not provenance.is_symlink()):
        return False
    try:
        recorded = json.loads(provenance.read_text())
        return (recorded["identity"] == identity
                and recorded["binary_sha256"] == digest(binary.read_bytes())
                and os.access(binary, os.X_OK))
    except (OSError, ValueError, KeyError, TypeError):
        return False


def selected_entry(cache, key, identity):
    selector = cache / f"{key}.current"
    if selector.is_symlink():
        name = os.readlink(selector)
        version = name.removeprefix(f"{key}.v")
        if len(version) != 32 or any(char not in "0123456789abcdef" for char in version):
            return None
        entry = cache / name
    elif selector.exists():
        return None
    else:
        # Readers published before versioned entries remain valid and in place.
        entry = cache / key
    return entry if verified_entry(entry, identity) else None


def main(repository, commit, reader_root, mode="offline", evidence="none"):
    if mode not in {"offline", "online"}:
        raise ValueError(f"unsupported historical reader build mode: {mode}")
    if evidence not in {"none", "archive", "root"}:
        raise ValueError(f"unsupported historical reader source evidence: {evidence}")
    repository = Path(repository).resolve()
    reader_root = Path(reader_root).resolve()
    # Cargo reads configuration from its working directory and its ancestors.
    # Build outside every worktree so the caller cannot change these inputs.
    build_cwd = Path(tempfile.gettempdir()).resolve()
    git_dir = Path(run("git", "rev-parse", "--path-format=absolute", "--git-common-dir", cwd=repository).decode().strip())
    cache = Path(os.environ.get("FLUENT_HISTORICAL_READER_CACHE", git_dir.parent / ".fluent/work/cache/historical-readers"))
    archive = run("git", "archive", commit, cwd=repository)
    with tarfile.open(fileobj=io.BytesIO(archive)) as source:
        lockfile = source.extractfile("Cargo.lock")
        work_model = source.extractfile("src/work_model.rs")
        if lockfile is None or work_model is None:
            raise ValueError("source archive lacks Cargo.lock or src/work_model.rs")
        lock_digest = digest(lockfile.read())
        work_model_bytes = work_model.read()
        work_model_digest = digest(work_model_bytes)
        # Some predecessors carry the pins for host tooling but do not link the
        # terminal adapter. Preserve their existing build identity and inputs.
        terminal_pin = (source.extractfile("tools/terminal-deps.env").read()
                        if b"target/terminal-deps/install" in source.extractfile("build.rs").read()
                        else None)
        root_files = {name: source.extractfile(name).read()
                      for name in ("Cargo.toml", "Cargo.lock", "src/main.rs")} if evidence == "root" else {}
    rustc = run("rustc", "-vV").decode()
    host = next((line.removeprefix("host: ").strip() for line in rustc.splitlines()
                 if line.startswith("host: ")), None)
    if not host:
        raise ValueError("rustc -vV did not report a host target")
    configured_target = os.environ.get("CARGO_BUILD_TARGET")
    if configured_target is not None and (not configured_target
                                          or Path(configured_target).name != configured_target
                                          or configured_target in {".", ".."}):
        raise ValueError(f"invalid CARGO_BUILD_TARGET triple: {configured_target!r}")
    build_target = configured_target if configured_target is not None else host
    terminal_root = repository / "target/terminal-deps/install" / build_target
    terminal_inputs = {}
    if terminal_pin is not None:
        if (repository / "tools/terminal-deps.env").read_bytes() != terminal_pin:
            raise ValueError("historical terminal pins differ; prepare those exact inputs separately")
        terminal_inputs = {str(path.relative_to(terminal_root)): digest(path.read_bytes())
                           for path in sorted(terminal_root.rglob("*")) if path.is_file()}
        if not terminal_inputs:
            raise ValueError("prepare pinned terminal inputs before building this historical reader")
    cargo = run("cargo", "--version").decode()
    options = {key: value for key, value in os.environ.items()
               if key.startswith(("RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC",
                                  "CARGO_BUILD_", "CARGO_PROFILE_", "CARGO_TARGET_", "CARGO_ENCODED_"))
               or key in {"CARGO_INCREMENTAL", "RUSTUP_TOOLCHAIN", "CC", "CFLAGS", "AR",
                          "SDKROOT", "MACOSX_DEPLOYMENT_TARGET"}}
    cargo_home = os.environ.get("CARGO_HOME")
    cargo_home_path = Path(cargo_home).resolve() if cargo_home else reader_root / "cargo-home"
    if not cargo_home:
        # Keep isolated preparation independent of the operator's Cargo home.
        cargo_home_path.mkdir(parents=True, exist_ok=True)
    cargo_config = {name: checked_config_digest(cargo_home_path / name)
                    for name in ("config.toml", "config") if (cargo_home_path / name).is_file()}
    build_command = ["cargo", "build", "--locked"]
    if mode == "offline":
        build_command.append("--offline")
    identity = {"source_commit": commit, "source_archive_sha256": digest(archive),
                "lockfile_sha256": lock_digest, "work_model_sha256": work_model_digest,
                "rustc": rustc, "cargo": cargo, "build_options": options,
                # A config-free Cargo home is portable across Attempts. Keep
                # the path when config values may resolve relative to it.
                "cargo_home": str(cargo_home_path) if cargo_config else None,
                "cargo_config": cargo_config,
                "cargo_cwd_config": cargo_cwd_config(build_cwd),
                "build_command": build_command}
    if terminal_pin is not None:
        identity["terminal_inputs"] = terminal_inputs
    key = digest(json.dumps(identity, sort_keys=True).encode())
    try:
        cache.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryFile(dir=cache):
            pass
    except PermissionError:
        # Writer sandboxes may only write their worktree. They retain isolated
        # preparation while the Tester profile grants the shared project cache.
        cache = reader_root / "cache"
        cache.mkdir(parents=True, exist_ok=True)
    with (cache / f"{key}.lock").open("a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        entry = selected_entry(cache, key, identity)
        if entry is None:
            with tempfile.TemporaryDirectory(prefix=f".{key}.", dir=cache) as temporary:
                temporary = Path(temporary)
                source_dir = temporary / "source"
                source_dir.mkdir()
                with tarfile.open(fileobj=io.BytesIO(archive)) as source:
                    source.extractall(source_dir)
                for name, expected in terminal_inputs.items():
                    content = (terminal_root / name).read_bytes()
                    if digest(content) != expected:
                        raise ValueError(f"terminal input changed during preparation: {name}")
                    destination = source_dir / "target/terminal-deps/install" / build_target / name
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination.write_bytes(content)
                target = temporary / "target"
                env = os.environ.copy()
                env["CARGO_HOME"] = str(cargo_home_path)
                env["GIT_CEILING_DIRECTORIES"] = str(source_dir.parent)
                try:
                    subprocess.run([*build_command, "--manifest-path", str(source_dir / "Cargo.toml"), "--target-dir", str(target)], cwd=build_cwd, env=env, check=True, stdout=sys.stderr, stderr=sys.stderr)
                except subprocess.CalledProcessError as error:
                    raise RuntimeError(f"build identity {key} failed: {error}") from error
                # Cargo nests artifacts under the target triple whenever the
                # target is explicit, including when it equals the host.
                output_root = target / build_target if configured_target is not None else target
                built = output_root / "debug/fluent"
                if configured_target is None:
                    # Cargo configuration can also select a target. The fresh
                    # target directory must contain exactly one reader binary.
                    candidates = [path for path in (built, *target.glob("*/debug/fluent"))
                                  if path.is_file()]
                    if len(candidates) != 1:
                        raise ValueError(
                            f"build identity {key}: executable verification failed: "
                            f"expected one Cargo reader, found {len(candidates)}")
                    built = candidates[0]
                if (not built.is_file() or not os.access(built, os.X_OK)
                        or not built.resolve().is_relative_to(target.resolve())):
                    raise ValueError(f"build identity {key}: executable verification failed: {built}")
                staged = temporary / "entry"
                staged.mkdir()
                shutil.copy2(built, staged / "fluent")
                (staged / "provenance.json").write_text(json.dumps({"identity": identity, "binary_sha256": digest((staged / "fluent").read_bytes())}, indent=2) + "\n")
                entry = cache / f"{key}.v{uuid.uuid4().hex}"
                staged.rename(entry)
                selector = cache / f"{key}.current"
                staged_selector = cache / f".{key}.{uuid.uuid4().hex}.tmp"
                try:
                    staged_selector.symlink_to(entry.name)
                    staged_selector.replace(selector)
                finally:
                    staged_selector.unlink(missing_ok=True)
        binary = entry / "fluent"
        # Preserve the fixture path and identity fields consumed by compatibility tests.
        if evidence == "archive":
            write_proof(reader_root / "source.tar", archive)
            write_proof(reader_root / "source/src/work_model.rs", work_model_bytes)
        elif evidence == "root":
            for name, content in root_files.items():
                write_proof(reader_root / name, content)
        local_binary = reader_root / "target/debug/fluent"
        local_binary.parent.mkdir(parents=True, exist_ok=True)
        versioned_binary = local_binary.with_name(f"fluent.{entry.name}")
        if versioned_binary.is_symlink():
            if os.readlink(versioned_binary) != str(binary):
                raise ValueError(f"build identity {key}: returned executable path changed: {versioned_binary}")
        elif versioned_binary.exists():
            raise ValueError(f"build identity {key}: returned executable path is not a link: {versioned_binary}")
        else:
            versioned_binary.symlink_to(binary)
        # Older consumers inspect this conventional path after first preparation.
        # Never repoint it: a prior caller may already hold the path.
        try:
            local_binary.symlink_to(binary)
        except FileExistsError:
            pass
        fixture_identity = {"source_commit": commit, "source_archive_sha256": identity["source_archive_sha256"],
                            "work_model_sha256": work_model_digest, "binary_sha256": digest(binary.read_bytes())}
        reader_root.mkdir(parents=True, exist_ok=True)
        (reader_root / "identity.json").write_text(json.dumps(fixture_identity, indent=2) + "\n")
        print(versioned_binary)


if __name__ == "__main__":
    try:
        main(*sys.argv[1:])
    except Exception as error:
        print(f"historical reader preparation failed: {error}", file=sys.stderr)
        sys.exit(1)
