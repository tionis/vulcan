#!/usr/bin/env python3
"""Validate and sign the published rolling release from a trusted local machine."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import pathlib
import re
import stat
import subprocess
import tempfile
from typing import NamedTuple

import update_channel


MAIN_KEY_ID = "main-2026-09"
MAIN_PUBLIC_KEY = "6gbtjy5nGZoT8kFAfYELB5x73S34kjv+/tPn8XEjrg0="
TARGET_FORMATS = update_channel.TARGET_FORMATS
DEBIAN_TARGETS = {
    "aarch64-unknown-linux-gnu": "arm64",
    "x86_64-unknown-linux-gnu": "amd64",
}
ROLLING_VERSION = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+-dev\.[0-9]{8}\.[0-9]+\.g([0-9a-f]{8})$"
)


class ValidatedRelease(NamedTuple):
    version: str
    source_commit: str
    published_at: str
    payload: bytes
    descriptor: pathlib.Path


def run(arguments: list[str]) -> str:
    result = subprocess.run(
        arguments,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or "command failed without diagnostic output"
        raise ValueError(f"{arguments[0]} command failed: {detail}")
    return result.stdout


def gh_json(arguments: list[str]) -> object:
    output = run(["gh", *arguments])
    try:
        return json.loads(output)
    except json.JSONDecodeError as error:
        raise ValueError("GitHub CLI returned invalid JSON") from error


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: pathlib.Path, label: str) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid {label}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"invalid {label}: expected a JSON object")
    return value


def canonical_pretty(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def debian_version(version: str) -> str:
    core, separator, build = version.partition("+")
    core = core.replace("-", "~", 1)
    return core + (f"+{build}" if separator else "") + "-1"


def validate_release_snapshot(
    release: dict, tag_commit: str
) -> dict[str, tuple[int, int, str, str]]:
    if release.get("tag_name") != "main" or release.get("draft") is not False:
        raise ValueError("rolling release must be the published `main` release")
    if release.get("prerelease") is not True:
        raise ValueError("rolling release must be marked as a prerelease")
    if not isinstance(release.get("id"), int):
        raise ValueError("rolling release is missing its numeric release ID")
    if not re.fullmatch(r"[0-9a-f]{40}", tag_commit):
        raise ValueError("rolling tag did not resolve to a full lowercase commit ID")
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise ValueError("rolling release has no asset inventory")
    inventory: dict[str, tuple[int, int, str, str]] = {}
    logical_names: set[str] = set()
    for asset in assets:
        if not isinstance(asset, dict):
            raise ValueError("rolling release contains malformed asset metadata")
        name = asset.get("name")
        asset_id = asset.get("id")
        size = asset.get("size")
        updated_at = asset.get("updated_at")
        label = asset.get("label") or ""
        if (
            not isinstance(name, str)
            or pathlib.PurePath(name).name != name
            or not isinstance(asset_id, int)
            or not isinstance(size, int)
            or size <= 0
            or not isinstance(updated_at, str)
            or not isinstance(label, str)
            or (label and pathlib.PurePath(label).name != label)
        ):
            raise ValueError("rolling release contains invalid asset metadata")
        if name in inventory:
            raise ValueError(f"rolling release contains duplicate asset {name}")
        logical_name = label or name
        if logical_name in logical_names:
            raise ValueError(f"rolling release contains duplicate logical asset {logical_name}")
        logical_names.add(logical_name)
        inventory[name] = (asset_id, size, updated_at, label)
    return inventory


def validate_successful_runs(
    runs: object,
    source_commit: str,
    *,
    workflow: str,
    required_event: str | None = None,
) -> None:
    if not isinstance(runs, list):
        raise ValueError(f"{workflow} workflow query returned malformed data")
    matches = [
        entry
        for entry in runs
        if isinstance(entry, dict)
        and entry.get("headSha") == source_commit
        and entry.get("headBranch") == "main"
        and entry.get("status") == "completed"
        and entry.get("conclusion") == "success"
        and (required_event is None or entry.get("event") == required_event)
    ]
    if not matches:
        raise ValueError(f"no successful completed {workflow} run names {source_commit}")


def validate_key(signing_key: pathlib.Path) -> None:
    if signing_key.is_symlink() or not signing_key.is_file():
        raise ValueError("signing key must be a regular, non-symlink file")
    if os.name != "nt" and stat.S_IMODE(signing_key.stat().st_mode) & 0o077:
        raise ValueError("signing key must not be accessible by group or other users")
    public_der = subprocess.run(
        [
            "openssl",
            "pkey",
            "-in",
            str(signing_key),
            "-pubout",
            "-outform",
            "DER",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if public_der.returncode != 0:
        raise ValueError("OpenSSL could not read the rolling release signing key")
    expected = base64.b64decode(MAIN_PUBLIC_KEY, validate=True)
    if len(public_der.stdout) != 44 or public_der.stdout[-32:] != expected:
        raise ValueError("signing key does not match the compiled main-channel public key")


def validate_artifact_record(record: object, version: str) -> tuple[str, str]:
    if not isinstance(record, dict):
        raise ValueError("release manifest contains a malformed artifact record")
    kind = record.get("kind")
    target = record.get("target")
    if kind not in {"archive", "debian"} or not isinstance(target, str):
        raise ValueError("release manifest contains an unsupported artifact identity")
    expected_fields = {
        "schema_version",
        "kind",
        "name",
        "version",
        "target",
        "format",
        "sha256",
        "size",
        "contents",
    }
    if kind == "archive":
        expected_fields.add("top_level_directory")
    else:
        expected_fields.update({"package_version", "architecture"})
    if set(record) != expected_fields:
        raise ValueError(f"release manifest {kind} record has an unexpected schema")
    if record["schema_version"] != 1 or record["version"] != version:
        raise ValueError("release manifest artifact version or schema mismatch")
    name = record["name"]
    digest = record["sha256"]
    size = record["size"]
    contents = record["contents"]
    if (
        not isinstance(name, str)
        or pathlib.PurePath(name).name != name
        or not isinstance(digest, str)
        or not re.fullmatch(r"[0-9a-f]{64}", digest)
        or not isinstance(size, int)
        or size <= 0
        or not isinstance(contents, list)
        or not all(isinstance(path, str) and path for path in contents)
        or contents != sorted(set(contents))
    ):
        raise ValueError("release manifest contains invalid artifact integrity metadata")
    if kind == "archive":
        expected_format = TARGET_FORMATS.get(target)
        if (
            record["format"] != expected_format
            or record["top_level_directory"] != f"vulcan-{version}-{target}"
        ):
            raise ValueError(f"release archive layout mismatch for {target}")
    else:
        expected_architecture = DEBIAN_TARGETS.get(target)
        if (
            record["format"] != "deb"
            or record["architecture"] != expected_architecture
            or record["package_version"] != debian_version(version)
        ):
            raise ValueError(f"release Debian metadata mismatch for {target}")
    return kind, target


def validate_downloaded_release(
    directory: pathlib.Path,
    release: dict,
    tag_commit: str,
    repo: str,
    expected_commit: str | None = None,
) -> ValidatedRelease:
    inventory = validate_release_snapshot(release, tag_commit)
    downloaded = {path.name: path for path in directory.iterdir() if path.is_file()}
    if set(downloaded) != set(inventory):
        raise ValueError("downloaded files do not match the release asset inventory")
    for name, (_, expected_size, _, _) in inventory.items():
        if downloaded[name].stat().st_size != expected_size:
            raise ValueError(f"downloaded release asset size mismatch for {name}")

    logical_assets = {
        (label or name): downloaded[name]
        for name, (_, _, _, label) in inventory.items()
    }

    manifests = sorted(directory.glob("vulcan-*-manifest.json"))
    if len(manifests) != 1:
        raise ValueError("rolling release must contain exactly one Vulcan manifest")
    manifest_path = manifests[0]
    manifest = load_json(manifest_path, "release manifest")
    if set(manifest) != {"schema_version", "product", "version", "artifacts"}:
        raise ValueError("release manifest has an unexpected schema")
    if manifest["schema_version"] != 1 or manifest["product"] != "vulcan":
        raise ValueError("release manifest has an unsupported identity")
    version = manifest["version"]
    if not isinstance(version, str) or manifest_path.name != f"vulcan-{version}-manifest.json":
        raise ValueError("release manifest filename and version do not match")
    version_match = ROLLING_VERSION.fullmatch(version)
    if version_match is None or version_match.group(1) != tag_commit[:8]:
        raise ValueError("release version does not identify the rolling tag commit")
    if manifest_path.read_bytes() != canonical_pretty(manifest):
        raise ValueError("release manifest is not in canonical serialized form")
    artifacts = manifest["artifacts"]
    if not isinstance(artifacts, list) or not all(
        isinstance(record, dict) for record in artifacts
    ):
        raise ValueError("release manifest artifact list is malformed")
    validated_identities = [
        validate_artifact_record(record, version) for record in artifacts
    ]
    if [record["name"] for record in artifacts] != sorted(
        record["name"] for record in artifacts
    ):
        raise ValueError("release manifest artifact order is not canonical")
    identities = set(validated_identities)
    expected_identities = {
        ("archive", target) for target in TARGET_FORMATS
    } | {("debian", target) for target in DEBIAN_TARGETS}
    if identities != expected_identities or len(artifacts) != len(expected_identities):
        raise ValueError("release manifest does not contain the exact supported artifact set")
    for record in artifacts:
        artifact = logical_assets.get(record["name"])
        if (
            artifact is None
            or artifact.stat().st_size != record["size"]
            or sha256(artifact) != record["sha256"]
        ):
            raise ValueError(f"release artifact integrity mismatch for {record['name']}")
    expected_checksums = "".join(
        f"{record['sha256']}  {record['name']}\n" for record in artifacts
    ).encode("ascii")
    checksums = logical_assets.get("SHA256SUMS")
    if checksums is None or checksums.read_bytes() != expected_checksums:
        raise ValueError("SHA256SUMS does not exactly match the canonical manifest")

    descriptor = logical_assets.get("vulcan-update-channel.json")
    if descriptor is None:
        raise ValueError("rolling release is missing vulcan-update-channel.json")
    envelope = load_json(descriptor, "update-channel envelope")
    if set(envelope) != {"schema_version", "payload", "signatures"}:
        raise ValueError("update-channel envelope has an unexpected schema")
    if envelope["schema_version"] != 1 or not isinstance(envelope["signatures"], list):
        raise ValueError("update-channel envelope has an unsupported identity")
    try:
        payload_bytes = base64.b64decode(envelope["payload"], validate=True)
        payload = json.loads(payload_bytes)
    except (TypeError, ValueError, json.JSONDecodeError) as error:
        raise ValueError("update-channel envelope contains an invalid payload") from error
    if not isinstance(payload, dict) or payload_bytes != update_channel.canonical_payload(payload):
        raise ValueError("update-channel payload is not canonical JSON")
    source_commit = payload.get("source_commit")
    if source_commit != tag_commit or (expected_commit and source_commit != expected_commit):
        raise ValueError("update-channel source commit does not match the rolling tag")
    base_url = f"https://github.com/{repo}/releases/download/main"
    expected_payload = {
        "schema_version": 1,
        "product": "vulcan",
        "channel": "main",
        "version": version,
        "source_commit": tag_commit,
        "published_at": payload.get("published_at"),
        "prerelease": True,
        "artifacts": [
            {
                "target": record["target"],
                "kind": "archive",
                "format": record["format"],
                "url": f"{base_url}/{record['name']}",
                "sha256": record["sha256"],
                "size": record["size"],
                "top_level_directory": record["top_level_directory"],
            }
            for record in sorted(
                (record for record in artifacts if record["kind"] == "archive"),
                key=lambda record: record["target"],
            )
        ],
    }
    published_at = expected_payload["published_at"]
    if (
        not isinstance(published_at, str)
        or update_channel.normalize_timestamp(published_at) != published_at
        or payload != expected_payload
    ):
        raise ValueError("update-channel payload does not match the validated release")
    return ValidatedRelease(
        version=version,
        source_commit=source_commit,
        published_at=published_at,
        payload=payload_bytes,
        descriptor=descriptor,
    )


def signed_envelope(payload: bytes, signing_key: pathlib.Path, key_id: str) -> bytes:
    signature = update_channel.sign_payload(payload, signing_key)
    envelope = {
        "schema_version": 1,
        "payload": base64.b64encode(payload).decode("ascii"),
        "signatures": [
            {
                "algorithm": "ed25519",
                "key_id": key_id,
                "signature": base64.b64encode(signature).decode("ascii"),
            }
        ],
    }
    return canonical_pretty(envelope)


def already_signed_descriptor(
    descriptor: pathlib.Path,
    tag_commit: str,
    signing_key: pathlib.Path,
    key_id: str,
) -> ValidatedRelease | None:
    envelope = load_json(descriptor, "update-channel envelope")
    if set(envelope) != {"schema_version", "payload", "signatures"}:
        raise ValueError("update-channel envelope has an unexpected schema")
    signatures = envelope.get("signatures")
    if envelope.get("schema_version") != 1 or not isinstance(signatures, list):
        raise ValueError("update-channel envelope has an unsupported identity")
    if not signatures:
        return None
    try:
        payload_bytes = base64.b64decode(envelope["payload"], validate=True)
        payload = json.loads(payload_bytes)
    except (TypeError, ValueError, json.JSONDecodeError) as error:
        raise ValueError("update-channel envelope contains an invalid payload") from error
    if (
        not isinstance(payload, dict)
        or payload_bytes != update_channel.canonical_payload(payload)
        or payload.get("schema_version") != 1
        or payload.get("product") != "vulcan"
        or payload.get("channel") != "main"
        or payload.get("prerelease") is not True
        or payload.get("source_commit") != tag_commit
        or not isinstance(payload.get("version"), str)
        or not isinstance(payload.get("published_at"), str)
    ):
        raise ValueError("signed update-channel payload does not identify the rolling release")
    if descriptor.read_bytes() != signed_envelope(payload_bytes, signing_key, key_id):
        raise ValueError("refusing an update descriptor with unexpected signatures")
    return ValidatedRelease(
        version=payload["version"],
        source_commit=tag_commit,
        published_at=payload["published_at"],
        payload=payload_bytes,
        descriptor=descriptor,
    )


def release_snapshot(release: dict, tag_commit: str) -> tuple[int, str, dict]:
    return release["id"], tag_commit, validate_release_snapshot(release, tag_commit)


def fetch_release(repo: str) -> tuple[dict, str]:
    release = gh_json(["api", f"repos/{repo}/releases/tags/main"])
    tag = gh_json(["api", f"repos/{repo}/git/ref/tags/main"])
    if not isinstance(release, dict) or not isinstance(tag, dict):
        raise ValueError("GitHub returned malformed release or tag data")
    tag_object = tag.get("object")
    if not isinstance(tag_object, dict):
        raise ValueError("GitHub did not resolve the rolling tag to a commit")
    sha = tag_object.get("sha")
    object_type = tag_object.get("type")
    if object_type == "tag" and isinstance(sha, str):
        annotated = gh_json(["api", f"repos/{repo}/git/tags/{sha}"])
        if not isinstance(annotated, dict) or not isinstance(annotated.get("object"), dict):
            raise ValueError("GitHub returned malformed annotated tag data")
        sha = annotated["object"].get("sha")
        object_type = annotated["object"].get("type")
    if object_type != "commit" or not isinstance(sha, str):
        raise ValueError("rolling tag does not point to a Git commit")
    validate_release_snapshot(release, sha)
    return release, sha


def fetch_runs(repo: str, workflow: str, source_commit: str) -> object:
    return gh_json(
        [
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            workflow,
            "--commit",
            source_commit,
            "--limit",
            "30",
            "--json",
            "conclusion,event,headBranch,headSha,status,url",
        ]
    )


def sign_rolling_release(
    repo: str,
    signing_key: pathlib.Path,
    key_id: str,
    expected_commit: str | None,
    dry_run: bool,
) -> dict:
    if key_id != MAIN_KEY_ID:
        raise ValueError(f"rolling release signer requires key ID {MAIN_KEY_ID}")
    validate_key(signing_key)
    release, tag_commit = fetch_release(repo)
    if expected_commit and tag_commit != expected_commit:
        raise ValueError("rolling tag does not match --expected-commit")
    validate_successful_runs(
        fetch_runs(repo, "CI", tag_commit),
        tag_commit,
        workflow="CI",
        required_event="push",
    )
    validate_successful_runs(
        fetch_runs(repo, "rolling-release.yml", tag_commit),
        tag_commit,
        workflow="rolling release",
    )
    before = release_snapshot(release, tag_commit)
    with tempfile.TemporaryDirectory(prefix="vulcan-rolling-probe-") as probe:
        run(
            [
                "gh",
                "release",
                "download",
                "main",
                "--repo",
                repo,
                "--pattern",
                "vulcan-update-channel.json",
                "--dir",
                probe,
            ]
        )
        existing = already_signed_descriptor(
            pathlib.Path(probe, "vulcan-update-channel.json"),
            tag_commit,
            signing_key,
            key_id,
        )
    if existing is not None:
        return {
            "action": "already_signed",
            "repo": repo,
            "tag": "main",
            "version": existing.version,
            "source_commit": existing.source_commit,
            "key_id": key_id,
            "dry_run": dry_run,
        }
    with tempfile.TemporaryDirectory(prefix="vulcan-rolling-sign-") as temporary:
        directory = pathlib.Path(temporary)
        run(["gh", "release", "download", "main", "--repo", repo, "--dir", str(directory)])
        validated = validate_downloaded_release(
            directory, release, tag_commit, repo, expected_commit
        )
        signed = signed_envelope(validated.payload, signing_key, key_id)
        current = validated.descriptor.read_bytes()
        envelope = load_json(validated.descriptor, "update-channel envelope")
        if envelope["signatures"]:
            raise ValueError("refusing to replace an unexpected signed update descriptor")
        if current != canonical_pretty(envelope):
            raise ValueError("unsigned update-channel envelope is not canonical")
        action = "would_sign" if dry_run else "signed"
        if not dry_run:
            latest_release, latest_commit = fetch_release(repo)
            if release_snapshot(latest_release, latest_commit) != before:
                raise ValueError("rolling release changed while it was being validated")
            validated.descriptor.write_bytes(signed)
            run(
                [
                    "gh",
                    "release",
                    "upload",
                    "main",
                    str(validated.descriptor),
                    "--repo",
                    repo,
                    "--clobber",
                ]
            )
            with tempfile.TemporaryDirectory(prefix="vulcan-rolling-readback-") as readback:
                run(
                    [
                        "gh",
                        "release",
                        "download",
                        "main",
                        "--repo",
                        repo,
                        "--pattern",
                        "vulcan-update-channel.json",
                        "--dir",
                        readback,
                    ]
                )
                if pathlib.Path(readback, "vulcan-update-channel.json").read_bytes() != signed:
                    raise ValueError("signed descriptor readback did not match uploaded bytes")
    return {
        "action": action,
        "repo": repo,
        "tag": "main",
        "version": validated.version,
        "source_commit": validated.source_commit,
        "key_id": key_id,
        "dry_run": dry_run,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="tionis/vulcan")
    parser.add_argument("--signing-key", required=True, type=pathlib.Path)
    parser.add_argument("--key-id", default=MAIN_KEY_ID)
    parser.add_argument("--expected-commit")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args()
    report = sign_rolling_release(
        arguments.repo,
        arguments.signing_key.expanduser().resolve(),
        arguments.key_id,
        arguments.expected_commit,
        arguments.dry_run,
    )
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
