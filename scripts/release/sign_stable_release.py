#!/usr/bin/env python3
"""Approval-gated signer for one immutable Vulcan stable release."""

from __future__ import annotations

import argparse
import json
import pathlib
import re

from sign_rolling_release import sign_published_release


STABLE_KEY_ID = "stable-2026-09"
STABLE_PUBLIC_KEY = "sOrBt76ruZ2kSR+4glX9k/ZjSoS1YSvmK9yMSVCiWpE="


def sign_stable_release(
    repo: str,
    tag: str,
    expected_commit: str,
    signing_key: pathlib.Path,
    key_id: str,
    dry_run: bool,
) -> dict:
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("stable release tag must be exactly v<major>.<minor>.<patch>")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_commit):
        raise ValueError("--expected-commit must be a full lowercase Git commit ID")
    return sign_published_release(
        repo,
        signing_key,
        key_id,
        expected_commit,
        dry_run,
        tag=tag,
        channel="stable",
        prerelease=False,
        release_kind="stable",
        expected_key_id=STABLE_KEY_ID,
        expected_public_key=STABLE_PUBLIC_KEY,
        required_runs=[("release.yml", "stable release", "push", tag)],
        fast_already_signed=False,
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="tionis/vulcan")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--expected-commit", required=True)
    parser.add_argument("--signing-key", required=True, type=pathlib.Path)
    parser.add_argument("--key-id", default=STABLE_KEY_ID)
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args()
    try:
        report = sign_stable_release(
            arguments.repo,
            arguments.tag,
            arguments.expected_commit,
            arguments.signing_key.expanduser().resolve(),
            arguments.key_id,
            arguments.dry_run,
        )
    except ValueError as error:
        parser.exit(1, f"error: {error}\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
