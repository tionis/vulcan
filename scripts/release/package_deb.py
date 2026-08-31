#!/usr/bin/env python3
"""Build a deterministic Debian package from a Vulcan Linux binary."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import re
import shutil
import stat
import tarfile
import tempfile


VERSION_PATTERN = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
TARGET_ARCHITECTURES = {
    "x86_64-unknown-linux-gnu": "amd64",
    "aarch64-unknown-linux-gnu": "arm64",
}
TARGET_ELF_MACHINES = {
    "x86_64-unknown-linux-gnu": 62,
    "aarch64-unknown-linux-gnu": 183,
}


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


def debian_version(version: str) -> str:
    if not VERSION_PATTERN.fullmatch(version):
        raise ValueError(f"invalid release version: {version}")
    core, separator, build = version.partition("+")
    core = core.replace("-", "~", 1)
    upstream = core + (f"+{build}" if separator else "")
    return f"{upstream}-1"


def validate_inputs(
    binary: pathlib.Path,
    assets: pathlib.Path,
    source: pathlib.Path,
    target: str,
    version: str,
) -> str:
    if not binary.is_file():
        raise ValueError(f"release binary does not exist: {binary}")
    if target not in TARGET_ARCHITECTURES:
        raise ValueError(f"unsupported Debian target: {target}")
    with binary.open("rb") as stream:
        header = stream.read(20)
    if (
        len(header) < 20
        or header[:4] != b"\x7fELF"
        or header[4] != 2
        or header[5] != 1
    ):
        raise ValueError(
            f"Debian package binary is not a 64-bit little-endian ELF file: {binary}"
        )
    machine = int.from_bytes(header[18:20], "little")
    if machine != TARGET_ELF_MACHINES[target]:
        raise ValueError(
            f"ELF machine {machine} does not match Debian target {target} "
            f"(expected {TARGET_ELF_MACHINES[target]})"
        )
    debian_version(version)
    required = [
        assets / "completions/vulcan.bash",
        assets / "completions/vulcan.fish",
        assets / "completions/_vulcan",
        assets / "vulcan.1",
        source / "README.md",
        source / "docs/installation.md",
        source / "LICENSE-MIT",
        source / "LICENSE-APACHE",
    ]
    missing = [str(path) for path in required if not path.is_file()]
    if missing:
        raise ValueError("missing Debian package inputs: " + ", ".join(missing))
    return TARGET_ARCHITECTURES[target]


def copy_file(source: pathlib.Path, destination: pathlib.Path, mode: int) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    os.chmod(destination, mode)


def gzip_file(source: pathlib.Path, destination: pathlib.Path, epoch: int) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with source.open("rb") as input_stream, destination.open("wb") as output_stream:
        with gzip.GzipFile(fileobj=output_stream, mode="wb", filename="", mtime=epoch) as compressed:
            shutil.copyfileobj(input_stream, compressed)
    os.chmod(destination, 0o644)


def stage_data(
    root: pathlib.Path,
    binary: pathlib.Path,
    assets: pathlib.Path,
    source: pathlib.Path,
    epoch: int,
) -> list[str]:
    copy_file(binary, root / "usr/bin/vulcan", 0o755)
    gzip_file(assets / "vulcan.1", root / "usr/share/man/man1/vulcan.1.gz", epoch)
    copy_file(
        assets / "completions/vulcan.bash",
        root / "usr/share/bash-completion/completions/vulcan",
        0o644,
    )
    copy_file(
        assets / "completions/vulcan.fish",
        root / "usr/share/fish/vendor_completions.d/vulcan.fish",
        0o644,
    )
    copy_file(
        assets / "completions/_vulcan",
        root / "usr/share/zsh/vendor-completions/_vulcan",
        0o644,
    )
    for source_name, destination_name in (
        ("README.md", "README.md"),
        ("docs/installation.md", "INSTALL.md"),
        ("LICENSE-MIT", "LICENSE-MIT"),
        ("LICENSE-APACHE", "LICENSE-APACHE"),
    ):
        copy_file(source / source_name, root / "usr/share/doc/vulcan" / destination_name, 0o644)
    copyright_path = root / "usr/share/doc/vulcan/copyright"
    copyright_path.write_text(
        "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
        "Upstream-Name: Vulcan\n"
        "Source: https://github.com/tionis/vulcan\n\n"
        "Files: *\n"
        "Copyright: 2026 Vulcan contributors\n"
        "License: MIT or Apache-2.0\n"
        " The complete license texts are installed as LICENSE-MIT and LICENSE-APACHE.\n",
        encoding="utf-8",
        newline="\n",
    )
    os.chmod(copyright_path, 0o644)
    return sorted(
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    )


def installed_size_kib(root: pathlib.Path) -> int:
    size = sum(path.stat().st_size for path in root.rglob("*") if path.is_file())
    return max(1, (size + 1023) // 1024)


def control_contents(version: str, architecture: str, installed_size: int) -> str:
    return (
        "Package: vulcan\n"
        f"Version: {debian_version(version)}\n"
        "Section: utils\n"
        "Priority: optional\n"
        f"Architecture: {architecture}\n"
        "Maintainer: Vulcan contributors <tionis@users.noreply.github.com>\n"
        "Depends: git, libc6, libgcc-s1\n"
        f"Installed-Size: {installed_size}\n"
        "Homepage: https://github.com/tionis/vulcan\n"
        "Description: local-first Markdown information hub\n"
        " Vulcan indexes, queries, automates, publishes, and synchronizes\n"
        " Obsidian vaults and plain Markdown directories.\n"
    )


def tar_gz_bytes(root: pathlib.Path, epoch: int) -> bytes:
    tar_buffer = io.BytesIO()
    with tarfile.open(fileobj=tar_buffer, mode="w", format=tarfile.GNU_FORMAT) as output:
        for path in sorted(root.rglob("*"), key=lambda candidate: candidate.as_posix()):
            relative = "./" + path.relative_to(root).as_posix()
            info = output.gettarinfo(str(path), arcname=relative)
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = epoch
            if info.isdir():
                info.mode = 0o755
            if info.isfile():
                with path.open("rb") as stream:
                    output.addfile(info, stream)
            else:
                output.addfile(info)
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", filename="", mtime=epoch) as output:
        output.write(tar_buffer.getvalue())
    return compressed.getvalue()


def ar_member(name: str, contents: bytes, epoch: int) -> bytes:
    encoded_name = f"{name}/"
    if len(encoded_name) > 16 or not encoded_name.isascii():
        raise ValueError(f"ar member name is not portable: {name}")
    header = (
        f"{encoded_name:<16}{epoch:<12}{0:<6}{0:<6}{format(0o100644, 'o'):<8}"
        f"{len(contents):<10}`\n"
    ).encode("ascii")
    if len(header) != 60:
        raise AssertionError("invalid ar header length")
    return header + contents + (b"\n" if len(contents) % 2 else b"")


def write_deb(
    package_path: pathlib.Path,
    control_archive: bytes,
    data_archive: bytes,
    epoch: int,
) -> None:
    with package_path.open("wb") as output:
        output.write(b"!<arch>\n")
        output.write(ar_member("debian-binary", b"2.0\n", epoch))
        output.write(ar_member("control.tar.gz", control_archive, epoch))
        output.write(ar_member("data.tar.gz", data_archive, epoch))


def package(
    binary: pathlib.Path,
    assets: pathlib.Path,
    source: pathlib.Path,
    output: pathlib.Path,
    target: str,
    version: str,
) -> pathlib.Path:
    architecture = validate_inputs(binary, assets, source, target, version)
    output.mkdir(parents=True, exist_ok=True)
    package_version = debian_version(version)
    package_path = output / f"vulcan_{package_version}_{architecture}.deb"
    epoch = normalized_epoch()
    with tempfile.TemporaryDirectory(dir=output) as temporary_raw:
        temporary = pathlib.Path(temporary_raw)
        data_root = temporary / "data"
        control_root = temporary / "control"
        data_root.mkdir()
        control_root.mkdir()
        contents = stage_data(data_root, binary, assets, source, epoch)
        control = control_contents(version, architecture, installed_size_kib(data_root))
        (control_root / "control").write_text(control, encoding="utf-8", newline="\n")
        os.chmod(control_root / "control", 0o644)
        write_deb(
            package_path,
            tar_gz_bytes(control_root, epoch),
            tar_gz_bytes(data_root, epoch),
            epoch,
        )
    digest = sha256(package_path)
    (output / f"{package_path.name}.sha256").write_text(
        f"{digest}  {package_path.name}\n", encoding="ascii", newline="\n"
    )
    record = {
        "schema_version": 1,
        "kind": "debian",
        "name": package_path.name,
        "version": version,
        "package_version": package_version,
        "target": target,
        "architecture": architecture,
        "format": "deb",
        "sha256": digest,
        "size": package_path.stat().st_size,
        "contents": contents,
    }
    (output / f"{package_path.name}.artifact.json").write_text(
        json.dumps(record, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    return package_path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--assets", required=True, type=pathlib.Path)
    parser.add_argument(
        "--source", default=pathlib.Path(__file__).resolve().parents[2], type=pathlib.Path
    )
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    arguments = parser.parse_args()
    package_path = package(
        arguments.binary.resolve(),
        arguments.assets.resolve(),
        arguments.source.resolve(),
        arguments.output.resolve(),
        arguments.target,
        arguments.version,
    )
    print(package_path)


if __name__ == "__main__":
    main()
