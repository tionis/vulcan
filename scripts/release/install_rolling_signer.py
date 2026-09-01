#!/usr/bin/env python3
"""Install the machine-local rolling release signer as a systemd user timer."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile


SERVICE_NAME = "vulcan-rolling-signer.service"
TIMER_NAME = "vulcan-rolling-signer.timer"


def systemd_quote(value: str) -> str:
    if any(character in value for character in "\r\n\0"):
        raise ValueError("systemd argument contains a control character")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render_service(
    python: pathlib.Path,
    signer: pathlib.Path,
    repo: str,
    signing_key: pathlib.Path,
) -> str:
    arguments = [
        python,
        signer,
        "--repo",
        pathlib.Path(repo),
        "--signing-key",
        signing_key,
    ]
    command = " ".join(systemd_quote(str(argument)) for argument in arguments)
    return f"""[Unit]
Description=Validate and sign the Vulcan rolling release
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart={command}
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
LockPersonality=true
MemoryDenyWriteExecute=true
"""


def render_timer() -> str:
    return f"""[Unit]
Description=Periodically sign a validated Vulcan rolling release

[Timer]
OnBootSec=5m
OnUnitActiveSec=1h
RandomizedDelaySec=5m
Persistent=true
Unit={SERVICE_NAME}

[Install]
WantedBy=timers.target
"""


def atomic_write(path: pathlib.Path, contents: bytes, mode: int) -> None:
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError(f"refusing to replace non-regular path {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
        temporary.write(contents)
        temporary.flush()
        os.fsync(temporary.fileno())
        temporary_path = pathlib.Path(temporary.name)
    os.chmod(temporary_path, mode)
    os.replace(temporary_path, path)


def systemctl(*arguments: str) -> None:
    result = subprocess.run(
        ["systemctl", "--user", *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or "systemctl failed without diagnostic output"
        raise ValueError(detail)


def install(
    source: pathlib.Path,
    unit_directory: pathlib.Path,
    libexec_directory: pathlib.Path,
    repo: str,
    signing_key: pathlib.Path,
    dry_run: bool,
) -> dict:
    if not signing_key.is_file() or signing_key.is_symlink():
        raise ValueError("signing key must be a regular, non-symlink file")
    signer_source = source / "sign_rolling_release.py"
    update_channel_source = source / "update_channel.py"
    if not signer_source.is_file() or not update_channel_source.is_file():
        raise ValueError("release signer sources are incomplete")
    signer = libexec_directory / signer_source.name
    update_channel = libexec_directory / update_channel_source.name
    service = unit_directory / SERVICE_NAME
    timer = unit_directory / TIMER_NAME
    service_contents = render_service(
        pathlib.Path(sys.executable), signer, repo, signing_key
    ).encode("utf-8")
    timer_contents = render_timer().encode("utf-8")
    report = {
        "action": "install",
        "dry_run": dry_run,
        "service": str(service),
        "timer": str(timer),
        "signer": str(signer),
        "signing_key": str(signing_key),
    }
    if dry_run:
        return report
    atomic_write(signer, signer_source.read_bytes(), 0o755)
    atomic_write(update_channel, update_channel_source.read_bytes(), 0o644)
    atomic_write(service, service_contents, 0o644)
    atomic_write(timer, timer_contents, 0o644)
    systemctl("daemon-reload")
    systemctl("enable", "--now", TIMER_NAME)
    systemctl("start", SERVICE_NAME)
    return report


def uninstall(
    unit_directory: pathlib.Path,
    libexec_directory: pathlib.Path,
    dry_run: bool,
) -> dict:
    paths = [
        unit_directory / SERVICE_NAME,
        unit_directory / TIMER_NAME,
        libexec_directory / "sign_rolling_release.py",
        libexec_directory / "update_channel.py",
    ]
    report = {
        "action": "uninstall",
        "dry_run": dry_run,
        "removed": [str(path) for path in paths if path.exists()],
    }
    if dry_run:
        return report
    systemctl("disable", "--now", TIMER_NAME)
    for path in paths:
        if path.is_symlink() or (path.exists() and not path.is_file()):
            raise ValueError(f"refusing to remove non-regular path {path}")
        if path.exists():
            path.unlink()
    if libexec_directory.exists() and not any(libexec_directory.iterdir()):
        libexec_directory.rmdir()
    systemctl("daemon-reload")
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("install", "uninstall"))
    parser.add_argument("--repo", default="tionis/vulcan")
    parser.add_argument("--signing-key", type=pathlib.Path)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--unit-directory",
        type=pathlib.Path,
        default=pathlib.Path("~/.config/systemd/user"),
    )
    parser.add_argument(
        "--libexec-directory",
        type=pathlib.Path,
        default=pathlib.Path("~/.local/libexec/vulcan-release-signer"),
    )
    arguments = parser.parse_args()
    unit_directory = arguments.unit_directory.expanduser().resolve()
    libexec_directory = arguments.libexec_directory.expanduser().resolve()
    source = pathlib.Path(__file__).resolve().parent
    if arguments.action == "install":
        if arguments.signing_key is None:
            parser.error("install requires --signing-key")
        report = install(
            source,
            unit_directory,
            libexec_directory,
            arguments.repo,
            arguments.signing_key.expanduser().resolve(),
            arguments.dry_run,
        )
    else:
        report = uninstall(unit_directory, libexec_directory, arguments.dry_run)
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
