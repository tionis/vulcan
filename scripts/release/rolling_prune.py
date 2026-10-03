#!/usr/bin/env python3
"""Select superseded rolling-release assets that are safe to delete.

The rolling build uploads its new artifacts beside the previous build and then
prunes. Until the signing workflow publishes the new canonical descriptor (and
GitHub's download path stops serving the old one), clients still receive the
previous signed descriptor, so the archives it references must survive this
prune. The following build removes them.
"""

from __future__ import annotations

import argparse
import base64
import json
import pathlib
import urllib.parse

import update_channel


def referenced_archives(descriptor: pathlib.Path | None, base_url: str) -> set[str]:
    """Return archive asset names the currently published descriptor points at."""
    if descriptor is None or not descriptor.is_file():
        return set()
    try:
        envelope = json.loads(descriptor.read_text(encoding="utf-8"))
        payload = json.loads(base64.b64decode(envelope["payload"], validate=True))
        artifacts = payload["artifacts"]
    except (OSError, KeyError, TypeError, ValueError) as error:
        raise ValueError(f"published update descriptor is unreadable: {error}") from error
    prefix = base_url.rstrip("/") + "/"
    names = set()
    for artifact in artifacts:
        url = artifact.get("url") if isinstance(artifact, dict) else None
        if not isinstance(url, str) or not url.startswith(prefix):
            raise ValueError("published update descriptor references a foreign archive URL")
        name = urllib.parse.unquote(url[len(prefix) :])
        if not name or pathlib.PurePosixPath(name).name != name:
            raise ValueError("published update descriptor references an invalid archive name")
        names.add(name)
    return names


def superseded_assets(
    release: dict,
    current: set[str],
    retained: set[str],
) -> list[tuple[int, str]]:
    """Return (asset id, name) pairs that belong to neither build generation."""
    keep = current | retained | {update_channel.CANONICAL_DESCRIPTOR}
    doomed = []
    for asset in release.get("assets", []):
        name = asset["name"]
        # GitHub sanitizes some uploaded names (notably Debian's `~`) while
        # softprops restores the original filename as the asset label.
        label = asset.get("label") or ""
        if name not in keep and label not in keep:
            doomed.append((asset["id"], name))
    return doomed


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--release-json", required=True, type=pathlib.Path)
    parser.add_argument("--artifacts", required=True, type=pathlib.Path)
    parser.add_argument("--published-descriptor", type=pathlib.Path)
    parser.add_argument("--base-url", required=True)
    arguments = parser.parse_args()
    try:
        release = json.loads(arguments.release_json.read_text(encoding="utf-8"))
        current = {path.name for path in arguments.artifacts.iterdir() if path.is_file()}
        retained = referenced_archives(arguments.published_descriptor, arguments.base_url)
        doomed = superseded_assets(release, current, retained)
    except (OSError, KeyError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")
    for asset_id, name in doomed:
        print(f"{asset_id}\t{name}")


if __name__ == "__main__":
    main()
