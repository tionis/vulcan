#!/usr/bin/env python3
"""Approval-gated signer for one immutable Vulcan stable release."""

from __future__ import annotations

import argparse
import json
import pathlib
import re

from sign_rolling_release import (
    ED25519_ALGORITHM,
    SSHSIG_ALGORITHM,
    Signer,
    sign_published_release,
)


STABLE_KEY_ID = "stable-2026-09"
STABLE_PUBLIC_KEY = "sOrBt76ruZ2kSR+4glX9k/ZjSoS1YSvmK9yMSVCiWpE="
# Hardware-held successor: the operator's OpenPGP-card key, exposed as an SSH
# key through ssh-agent and used via `ssh-keygen -Y sign`.
STABLE_CARD_KEY_ID = "stable-2026-10"
STABLE_CARD_PUBLIC_KEY = "dk91fcu3uqtH5h0q/FWJ/qjlBb65wGojrngIuiusEU4="
STABLE_KEYS = {
    STABLE_KEY_ID: (ED25519_ALGORITHM, STABLE_PUBLIC_KEY),
    STABLE_CARD_KEY_ID: (SSHSIG_ALGORITHM, STABLE_CARD_PUBLIC_KEY),
}


def sign_stable_release(
    repo: str,
    tag: str,
    expected_commit: str,
    signing_key: pathlib.Path | None,
    key_id: str,
    dry_run: bool,
    ssh_signing_key: pathlib.Path | None = None,
) -> dict:
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("stable release tag must be exactly v<major>.<minor>.<patch>")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_commit):
        raise ValueError("--expected-commit must be a full lowercase Git commit ID")
    # During a rotation overlap both keys sign, so binaries that trust only one
    # of them can still verify the release.
    signers = []
    if signing_key is not None:
        signers.append(Signer(key_id, ED25519_ALGORITHM, signing_key))
    if ssh_signing_key is not None:
        signers.append(Signer(STABLE_CARD_KEY_ID, SSHSIG_ALGORITHM, ssh_signing_key))
    return sign_published_release(
        repo,
        signers,
        expected_commit,
        dry_run,
        tag=tag,
        channel="stable",
        prerelease=False,
        release_kind="stable",
        expected_keys=STABLE_KEYS,
        required_runs=[("release.yml", "stable release", "push", tag)],
        fast_already_signed=False,
        tag_is_source=True,
        promote_latest=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="tionis/vulcan")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--expected-commit", required=True)
    parser.add_argument(
        "--signing-key",
        type=pathlib.Path,
        help=f"PEM private key for {STABLE_KEY_ID}",
    )
    parser.add_argument("--key-id", default=STABLE_KEY_ID)
    parser.add_argument(
        "--ssh-signing-key",
        type=pathlib.Path,
        help=(
            f"SSH public key file for {STABLE_CARD_KEY_ID}; ssh-agent must hold the "
            "private half (for example on an OpenPGP card)"
        ),
    )
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args()
    try:
        report = sign_stable_release(
            arguments.repo,
            arguments.tag,
            arguments.expected_commit,
            (
                arguments.signing_key.expanduser().resolve()
                if arguments.signing_key
                else None
            ),
            arguments.key_id,
            arguments.dry_run,
            (
                arguments.ssh_signing_key.expanduser().resolve()
                if arguments.ssh_signing_key
                else None
            ),
        )
    except ValueError as error:
        parser.exit(1, f"error: {error}\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
