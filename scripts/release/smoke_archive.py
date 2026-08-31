#!/usr/bin/env python3
"""Verify and smoke-test one native Vulcan release archive."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import signal
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile


def extract(archive: pathlib.Path, destination: pathlib.Path) -> None:
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as packaged:
            packaged.extractall(destination)
    else:
        with tarfile.open(archive, "r:gz") as packaged:
            members = packaged.getmembers()
            if any(
                pathlib.PurePosixPath(member.name).is_absolute()
                or ".." in pathlib.PurePosixPath(member.name).parts
                for member in members
            ):
                raise ValueError("archive contains an unsafe path")
            packaged.extractall(destination, members=members, filter="data")


def run(binary: pathlib.Path, *arguments: str, environment: dict[str, str] | None = None) -> str:
    return subprocess.run(
        [str(binary), *arguments],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    ).stdout


def daemon_status(binary: pathlib.Path, environment: dict[str, str]) -> dict:
    return json.loads(
        run(binary, "--output", "json", "daemon", "status", environment=environment)
    )


def wait_for_daemon(
    binary: pathlib.Path,
    environment: dict[str, str],
    predicate,
    detail: str,
) -> dict:
    deadline = time.monotonic() + 20
    latest = {}
    while time.monotonic() < deadline:
        latest = daemon_status(binary, environment)
        if predicate(latest):
            return latest
        time.sleep(0.25)
    raise ValueError(f"timed out waiting for daemon {detail}: {latest}")


def smoke_macos_service(
    binary: pathlib.Path,
    environment: dict[str, str],
    definition_path: pathlib.Path,
) -> None:
    try:
        run(
            binary,
            "--output",
            "json",
            "daemon",
            "install",
            environment=environment,
        )
        first = wait_for_daemon(
            binary, environment, lambda status: status["running"], "startup"
        )
        first_pid = first["runtime"]["pid"]
        os.kill(first_pid, signal.SIGKILL)
        wait_for_daemon(
            binary,
            environment,
            lambda status: status["running"]
            and status["runtime"]["pid"] != first_pid,
            "restart after failure",
        )
        staged_binary = binary.with_suffix(".upgrade")
        shutil.copy2(binary, staged_binary)
        os.replace(staged_binary, binary)
        run(
            binary,
            "--output",
            "json",
            "daemon",
            "install",
            environment=environment,
        )
        wait_for_daemon(binary, environment, lambda status: status["running"], "reinstall")
        run(binary, "--output", "json", "daemon", "stop", environment=environment)
        wait_for_daemon(
            binary, environment, lambda status: not status["running"], "clean stop"
        )
    finally:
        run(
            binary,
            "--output",
            "json",
            "daemon",
            "uninstall",
            environment=environment,
        )
    if definition_path.exists():
        raise ValueError("macOS daemon uninstall retained the LaunchAgent definition")


def smoke_debian_package(
    directory: pathlib.Path,
    target: str,
    version: str,
    destination: pathlib.Path,
    environment: dict[str, str],
) -> None:
    records = [
        json.loads(path.read_text(encoding="utf-8"))
        for path in directory.glob("*.artifact.json")
    ]
    matching = [
        record
        for record in records
        if record.get("kind") == "debian" and record["target"] == target
    ]
    if len(matching) != 1:
        raise ValueError(f"expected one Debian artifact for {target}, found {len(matching)}")
    record = matching[0]
    package = directory / record["name"]
    if hashlib.sha256(package.read_bytes()).hexdigest() != record["sha256"]:
        raise ValueError("Debian package checksum does not match its artifact record")
    fields = subprocess.run(
        ["dpkg-deb", "--field", str(package), "Package", "Version", "Architecture", "Depends"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    ).stdout
    if "Package: vulcan" not in fields or f"Version: {record['package_version']}" not in fields:
        raise ValueError(f"unexpected Debian package metadata: {fields}")
    package_root = destination / "debian-package"
    subprocess.run(
        ["dpkg-deb", "--extract", str(package), str(package_root)],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    packaged_binary = package_root / "usr/bin/vulcan"
    reported = run(packaged_binary, "--version", environment=environment).strip()
    if reported != f"vulcan {version}":
        raise ValueError(f"Debian binary version mismatch: {reported}")
    if not (package_root / "usr/share/man/man1/vulcan.1.gz").is_file():
        raise ValueError("Debian package is missing the compressed man page")


def smoke(directory: pathlib.Path, target: str, version: str) -> None:
    extension = ".zip" if "windows" in target else ".tar.gz"
    archive = directory / f"vulcan-{version}-{target}{extension}"
    record = json.loads(
        (directory / f"{archive.name}.artifact.json").read_text(encoding="utf-8")
    )
    if hashlib.sha256(archive.read_bytes()).hexdigest() != record["sha256"]:
        raise ValueError("archive checksum does not match its artifact record")
    with tempfile.TemporaryDirectory() as temporary_raw:
        temporary = pathlib.Path(temporary_raw)
        extract(archive, temporary)
        root = temporary / record["top_level_directory"]
        binary = root / ("vulcan.exe" if "windows" in target else "vulcan")
        if not os.access(binary, os.X_OK):
            os.chmod(binary, 0o755)
        reported = run(binary, "--version").strip()
        if reported != f"vulcan {version}":
            raise ValueError(f"binary version mismatch: {reported}")
        environment = os.environ.copy()
        environment["HOME"] = str(temporary / "home")
        environment["USERPROFILE"] = environment["HOME"]
        environment["XDG_CONFIG_HOME"] = str(temporary / "config")
        environment["XDG_STATE_HOME"] = str(temporary / "state")
        if sys.platform == "darwin":
            stable_binary = temporary / "install/bin/vulcan"
            stable_binary.parent.mkdir(parents=True)
            shutil.copy2(binary, stable_binary)
            binary = stable_binary
        plan = json.loads(
            run(
                binary,
                "--output",
                "json",
                "daemon",
                "install",
                "--dry-run",
                environment=environment,
            )
        )
        if not plan["dry_run"] or plan["changed"]:
            raise ValueError("daemon service dry-run unexpectedly planned a mutation")
        if sys.platform == "darwin":
            smoke_macos_service(
                binary, environment, pathlib.Path(plan["definition_path"])
            )
        elif sys.platform.startswith("linux") and target == "x86_64-unknown-linux-gnu":
            smoke_debian_package(directory, target, version, temporary, environment)
        vault = temporary / "vault"
        vault.mkdir()
        subprocess.run(["git", "-C", str(vault), "init", "-q"], check=True)
        doctor = json.loads(
            run(
                binary,
                "--vault",
                str(vault),
                "--output",
                "json",
                "sync",
                "doctor",
                environment=environment,
            )
        )
        if doctor["installation"]["engine"] != "cli":
            raise ValueError("sync doctor did not report the Git CLI backend")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", required=True, type=pathlib.Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    arguments = parser.parse_args()
    smoke(arguments.directory.resolve(), arguments.target, arguments.version)


if __name__ == "__main__":
    main()
