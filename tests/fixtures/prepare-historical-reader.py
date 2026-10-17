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


def digest(data):
    return hashlib.sha256(data).hexdigest()


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd)


def write_proof(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as staged:
        staged.write(content)
    Path(staged.name).replace(path)


def main(repository, commit, reader_root, mode="offline", evidence="none"):
    if mode not in {"offline", "online"}:
        raise ValueError(f"unsupported historical reader build mode: {mode}")
    if evidence not in {"none", "archive", "root"}:
        raise ValueError(f"unsupported historical reader source evidence: {evidence}")
    repository = Path(repository).resolve()
    reader_root = Path(reader_root).resolve()
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
        root_files = {name: source.extractfile(name).read()
                      for name in ("Cargo.toml", "Cargo.lock", "src/main.rs")} if evidence == "root" else {}
    rustc = run("rustc", "-vV").decode()
    cargo = run("cargo", "--version").decode()
    options = {key: value for key, value in os.environ.items()
               if key.startswith(("RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC",
                                  "CARGO_BUILD_", "CARGO_PROFILE_", "CARGO_TARGET_", "CARGO_ENCODED_"))
               or key in {"CARGO_INCREMENTAL", "RUSTUP_TOOLCHAIN", "CC", "CFLAGS", "AR",
                          "SDKROOT", "MACOSX_DEPLOYMENT_TARGET"}}
    cargo_home = os.environ.get("CARGO_HOME")
    cargo_config = ({name: digest((Path(cargo_home) / name).read_bytes())
                     for name in ("config.toml", "config") if (Path(cargo_home) / name).is_file()}
                    if cargo_home else {})
    build_command = ["cargo", "build", "--locked"]
    if mode == "offline":
        build_command.append("--offline")
    identity = {"source_commit": commit, "source_archive_sha256": digest(archive),
                "lockfile_sha256": lock_digest, "work_model_sha256": work_model_digest,
                "rustc": rustc, "cargo": cargo, "build_options": options,
                "cargo_config": cargo_config,
                "build_command": build_command}
    key = digest(json.dumps(identity, sort_keys=True).encode())
    if not cargo_home:
        # Without an admitted Cargo home, local Cargo configuration cannot be
        # verified without reading the operator's global store.
        cache = reader_root / "cache"
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
        entry = cache / key
        binary = entry / "fluent"
        provenance = entry / "provenance.json"
        valid = False
        if entry.is_dir() and not entry.is_symlink() and binary.is_file() and not binary.is_symlink() and provenance.is_file() and not provenance.is_symlink():
            try:
                recorded = json.loads(provenance.read_text())
                valid = recorded["identity"] == identity and recorded["binary_sha256"] == digest(binary.read_bytes()) and os.access(binary, os.X_OK)
            except (OSError, ValueError, KeyError, TypeError):
                pass
        if not valid:
            if entry.exists():
                if entry.is_symlink() or not entry.is_dir():
                    entry.unlink()
                else:
                    shutil.rmtree(entry)
            with tempfile.TemporaryDirectory(prefix=f".{key}.", dir=cache) as temporary:
                temporary = Path(temporary)
                source_dir = temporary / "source"
                source_dir.mkdir()
                with tarfile.open(fileobj=io.BytesIO(archive)) as source:
                    source.extractall(source_dir)
                target = temporary / "target"
                env = os.environ.copy()
                env["GIT_CEILING_DIRECTORIES"] = str(source_dir.parent)
                try:
                    subprocess.run([*build_command, "--manifest-path", str(source_dir / "Cargo.toml"), "--target-dir", str(target)], env=env, check=True, stdout=sys.stderr, stderr=sys.stderr)
                except subprocess.CalledProcessError as error:
                    raise RuntimeError(f"build identity {key} failed: {error}") from error
                built = target / "debug/fluent"
                if not built.is_file() or not os.access(built, os.X_OK):
                    raise ValueError(f"build identity {key}: executable verification failed: {built}")
                staged = temporary / "entry"
                staged.mkdir()
                shutil.copy2(built, staged / "fluent")
                (staged / "provenance.json").write_text(json.dumps({"identity": identity, "binary_sha256": digest((staged / "fluent").read_bytes())}, indent=2) + "\n")
                staged.rename(entry)
        # Preserve the fixture path and identity fields consumed by compatibility tests.
        if evidence == "archive":
            write_proof(reader_root / "source.tar", archive)
            write_proof(reader_root / "source/src/work_model.rs", work_model_bytes)
        elif evidence == "root":
            for name, content in root_files.items():
                write_proof(reader_root / name, content)
        local_binary = reader_root / "target/debug/fluent"
        local_binary.parent.mkdir(parents=True, exist_ok=True)
        link = local_binary.with_name(f"fluent.{os.getpid()}.tmp")
        link.unlink(missing_ok=True)
        link.symlink_to(binary)
        link.replace(local_binary)
        fixture_identity = {"source_commit": commit, "source_archive_sha256": identity["source_archive_sha256"],
                            "work_model_sha256": work_model_digest, "binary_sha256": digest(binary.read_bytes())}
        reader_root.mkdir(parents=True, exist_ok=True)
        (reader_root / "identity.json").write_text(json.dumps(fixture_identity, indent=2) + "\n")
        print(local_binary)


if __name__ == "__main__":
    try:
        main(*sys.argv[1:])
    except Exception as error:
        print(f"historical reader preparation failed: {error}", file=sys.stderr)
        sys.exit(1)
