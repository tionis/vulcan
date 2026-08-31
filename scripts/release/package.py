#!/usr/bin/env python3
"""Build a deterministic, forge-neutral Vulcan release archive."""

from __future__ import annotations

import argparse
import datetime
import gzip
import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import tarfile
import tempfile
import zipfile


TARGET_PATTERN = re.compile(r"^[a-z0-9_]+(?:-[a-z0-9_.]+)+$")
VERSION_PATTERN = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
ASSET_FILES = (
    "completions/vulcan.bash",
    "completions/vulcan.fish",
    "completions/_vulcan",
    "completions/_vulcan.ps1",
    "completions/vulcan.elv",
    "vulcan.1",
)


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def normalized_epoch() -> int:
    raw = os.environ.get("SOURCE_DATE_EPOCH", "0")
    try:
        return max(0, int(raw))
    except ValueError as error:
        raise ValueError("SOURCE_DATE_EPOCH must be a non-negative integer") from error


def validate_inputs(
    binary: pathlib.Path,
    assets: pathlib.Path,
    source: pathlib.Path,
    target: str,
    version: str,
) -> None:
    if not binary.is_file():
        raise ValueError(f"release binary does not exist: {binary}")
    if not TARGET_PATTERN.fullmatch(target):
        raise ValueError(f"invalid Rust target triple: {target}")
    if not VERSION_PATTERN.fullmatch(version):
        raise ValueError(f"invalid release version: {version}")
    required = [assets / name for name in ASSET_FILES]
    required.extend(
        source / name
        for name in ("README.md", "docs/installation.md", "LICENSE-MIT", "LICENSE-APACHE")
    )
    missing = [str(path) for path in required if not path.is_file()]
    if missing:
        raise ValueError("missing release inputs: " + ", ".join(missing))


def stage_release(
    root: pathlib.Path,
    binary: pathlib.Path,
    assets: pathlib.Path,
    source: pathlib.Path,
    windows: bool,
) -> list[str]:
    binary_name = "vulcan.exe" if windows else "vulcan"
    shutil.copyfile(binary, root / binary_name)
    os.chmod(root / binary_name, 0o755)
    shutil.copytree(assets / "completions", root / "completions")
    shutil.copyfile(assets / "vulcan.1", root / "vulcan.1")
    shutil.copyfile(source / "README.md", root / "README.md")
    shutil.copyfile(source / "docs/installation.md", root / "INSTALL.md")
    shutil.copyfile(source / "LICENSE-MIT", root / "LICENSE-MIT")
    shutil.copyfile(source / "LICENSE-APACHE", root / "LICENSE-APACHE")
    return sorted(
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file()
    )


def write_tar_gz(archive: pathlib.Path, root: pathlib.Path, epoch: int) -> None:
    temporary_tar = archive.with_suffix("")
    with tarfile.open(temporary_tar, "w", format=tarfile.PAX_FORMAT) as output:
        for path in [root, *sorted(root.rglob("*"))]:
            relative = path.relative_to(root.parent).as_posix()
            info = output.gettarinfo(str(path), arcname=relative)
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = epoch
            if info.isfile():
                with path.open("rb") as stream:
                    output.addfile(info, stream)
            else:
                output.addfile(info)
    with temporary_tar.open("rb") as source, archive.open("wb") as destination:
        with gzip.GzipFile(fileobj=destination, mode="wb", filename="", mtime=epoch) as compressed:
            shutil.copyfileobj(source, compressed)
    temporary_tar.unlink()


def write_zip(archive: pathlib.Path, root: pathlib.Path, epoch: int) -> None:
    # ZIP timestamps cannot represent dates before 1980.
    timestamp = max(epoch, 315532800)
    date_time = datetime.datetime.fromtimestamp(
        timestamp, tz=datetime.timezone.utc
    ).timetuple()[:6]
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as output:
        for path in sorted(root.rglob("*")):
            if not path.is_file():
                continue
            relative = path.relative_to(root.parent).as_posix()
            info = zipfile.ZipInfo(relative, date_time=date_time)
            mode = 0o755 if path.name == "vulcan.exe" else 0o644
            info.external_attr = (stat.S_IFREG | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            output.writestr(info, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def package(
    binary: pathlib.Path,
    assets: pathlib.Path,
    source: pathlib.Path,
    output: pathlib.Path,
    target: str,
    version: str,
) -> pathlib.Path:
    validate_inputs(binary, assets, source, target, version)
    output.mkdir(parents=True, exist_ok=True)
    windows = "windows" in target
    extension = ".zip" if windows else ".tar.gz"
    base_name = f"vulcan-{version}-{target}"
    archive = output / f"{base_name}{extension}"
    epoch = normalized_epoch()
    with tempfile.TemporaryDirectory(dir=output) as temporary:
        root = pathlib.Path(temporary) / base_name
        root.mkdir()
        contents = stage_release(root, binary, assets, source, windows)
        if windows:
            write_zip(archive, root, epoch)
        else:
            write_tar_gz(archive, root, epoch)
    digest = sha256(archive)
    (output / f"{archive.name}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii", newline="\n"
    )
    manifest = {
        "schema_version": 1,
        "kind": "archive",
        "name": archive.name,
        "version": version,
        "target": target,
        "format": "zip" if windows else "tar.gz",
        "sha256": digest,
        "size": archive.stat().st_size,
        "top_level_directory": base_name,
        "contents": contents,
    }
    (output / f"{archive.name}.artifact.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n"
    )
    return archive


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--assets", required=True, type=pathlib.Path)
    parser.add_argument("--source", default=pathlib.Path(__file__).resolve().parents[2], type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    arguments = parser.parse_args()
    archive = package(
        arguments.binary.resolve(),
        arguments.assets.resolve(),
        arguments.source.resolve(),
        arguments.output.resolve(),
        arguments.target,
        arguments.version,
    )
    print(archive)


if __name__ == "__main__":
    main()
