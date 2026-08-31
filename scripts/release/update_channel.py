#!/usr/bin/env python3
"""Render the signed, forge-neutral Vulcan update-channel envelope."""

from __future__ import annotations

import argparse
import base64
import datetime
import json
import pathlib
import subprocess
import tempfile

TARGET_FORMATS = {
    "aarch64-apple-darwin": "tar.gz",
    "aarch64-unknown-linux-gnu": "tar.gz",
    "x86_64-apple-darwin": "tar.gz",
    "x86_64-pc-windows-msvc": "zip",
    "x86_64-unknown-linux-gnu": "tar.gz",
}


def canonical_payload(payload: dict) -> bytes:
    return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sign_payload(payload: bytes, signing_key: pathlib.Path) -> bytes:
    with tempfile.NamedTemporaryFile() as payload_file:
        payload_file.write(payload)
        payload_file.flush()
        result = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-sign",
                "-rawin",
                "-inkey",
                str(signing_key),
                "-in",
                payload_file.name,
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    if result.returncode != 0:
        raise ValueError(
            "OpenSSL could not sign the update channel: "
            + result.stderr.decode("utf-8", errors="replace").strip()
        )
    return result.stdout


def normalize_timestamp(value: str) -> str:
    parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("published-at must include a timezone")
    return parsed.astimezone(datetime.timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )


def generate(
    manifest_path: pathlib.Path,
    channel: str,
    base_url: str,
    source_commit: str,
    published_at: str,
    output: pathlib.Path,
    signing_key: pathlib.Path | None = None,
    key_id: str | None = None,
) -> pathlib.Path:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1 or manifest.get("product") != "vulcan":
        raise ValueError("unsupported release manifest")
    if channel not in {"stable", "main"}:
        raise ValueError(f"unsupported update channel: {channel}")
    if not base_url.startswith("https://"):
        raise ValueError("update artifact base URL must use HTTPS")
    version = manifest["version"]
    prerelease = "-" in version.split("+", 1)[0]
    if prerelease != (channel != "stable"):
        raise ValueError("manifest prerelease version does not match the update channel")
    if len(source_commit) != 40 or any(
        character not in "0123456789abcdefABCDEF" for character in source_commit
    ):
        raise ValueError("source commit must be a 40-character hexadecimal object id")
    archives = [
        artifact
        for artifact in manifest["artifacts"]
        if artifact.get("kind", "archive") == "archive"
    ]
    if len(archives) != 5:
        raise ValueError("update channels require exactly five portable archives")
    if {artifact["target"] for artifact in archives} != set(TARGET_FORMATS):
        raise ValueError("update channel archive targets do not match the supported target set")
    artifacts = []
    for artifact in sorted(archives, key=lambda record: record["target"]):
        target = artifact["target"]
        if artifact["format"] != TARGET_FORMATS[target]:
            raise ValueError(f"update archive format does not match target {target}")
        if artifact["top_level_directory"] != f"vulcan-{version}-{target}":
            raise ValueError(f"update archive layout does not match target {target}")
        artifacts.append(
            {
                "target": target,
                "kind": "archive",
                "format": artifact["format"],
                "url": f'{base_url.rstrip("/")}/{artifact["name"]}',
                "sha256": artifact["sha256"],
                "size": artifact["size"],
                "top_level_directory": artifact["top_level_directory"],
            }
        )
    payload = {
        "schema_version": 1,
        "product": "vulcan",
        "channel": channel,
        "version": version,
        "source_commit": source_commit.lower(),
        "published_at": normalize_timestamp(published_at),
        "prerelease": prerelease,
        "artifacts": artifacts,
    }
    payload_bytes = canonical_payload(payload)
    signatures = []
    if signing_key is not None:
        if not key_id:
            raise ValueError("key-id is required when signing an update channel")
        signatures.append(
            {
                "algorithm": "ed25519",
                "key_id": key_id,
                "signature": base64.b64encode(sign_payload(payload_bytes, signing_key)).decode(
                    "ascii"
                ),
            }
        )
    elif key_id:
        raise ValueError("signing-key is required when key-id is supplied")
    envelope = {
        "schema_version": 1,
        "payload": base64.b64encode(payload_bytes).decode("ascii"),
        "signatures": signatures,
    }
    output.mkdir(parents=True, exist_ok=True)
    destination = output / "vulcan-update-channel.json"
    destination.write_text(
        json.dumps(envelope, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    return destination


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True, type=pathlib.Path)
    parser.add_argument("--channel", required=True, choices=("stable", "main"))
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--published-at", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--signing-key", type=pathlib.Path)
    parser.add_argument("--key-id")
    arguments = parser.parse_args()
    destination = generate(
        arguments.manifest.resolve(),
        arguments.channel,
        arguments.base_url,
        arguments.source_commit,
        arguments.published_at,
        arguments.output.resolve(),
        arguments.signing_key.resolve() if arguments.signing_key else None,
        arguments.key_id,
    )
    print(destination)


if __name__ == "__main__":
    main()
