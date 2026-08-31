#!/usr/bin/env python3
"""Generate target-independent documentation shipped in Vulcan releases."""

from __future__ import annotations

import argparse
import os
import pathlib
import subprocess


COMPLETIONS = {
    "bash": "vulcan.bash",
    "fish": "vulcan.fish",
    "zsh": "_vulcan",
    "powershell": "_vulcan.ps1",
    "elvish": "vulcan.elv",
}


def run(binary: pathlib.Path, *arguments: str) -> str:
    environment = os.environ.copy()
    environment["VULCAN_COMPLETION_COMMAND"] = "vulcan"
    return subprocess.run(
        [str(binary), *arguments],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    ).stdout.replace("\r\n", "\n")


def roff_escape(line: str) -> str:
    escaped = line.replace("\\", r"\e")
    if escaped.startswith((".", "'")):
        escaped = r"\&" + escaped
    return escaped


def render_manpage(version: str, help_text: str) -> str:
    body = "\n".join(roff_escape(line) for line in help_text.rstrip().splitlines())
    return (
        f'.TH VULCAN 1 "" "Vulcan {version}" "User Commands"\n'
        ".SH NAME\n"
        "vulcan \\- local-first Markdown information hub\n"
        ".SH SYNOPSIS\n"
        ".B vulcan\n"
        "[OPTIONS] <COMMAND>\n"
        ".SH DESCRIPTION\n"
        "Vulcan indexes, queries, automates, publishes, and synchronizes Obsidian vaults "
        "and plain Markdown directories.\n"
        ".SH COMMAND HELP\n"
        ".nf\n"
        f"{body}\n"
        ".fi\n"
        ".SH SEE ALSO\n"
        "Project documentation is included in the release archive and published at "
        "https://github.com/tionis/vulcan.\n"
    )


def generate(binary: pathlib.Path, version: str, output: pathlib.Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    completions = output / "completions"
    completions.mkdir(exist_ok=True)
    for shell, filename in COMPLETIONS.items():
        (completions / filename).write_text(
            run(binary, "completions", shell), encoding="utf-8", newline="\n"
        )
    (output / "vulcan.1").write_text(
        render_manpage(version, run(binary, "--help")), encoding="utf-8", newline="\n"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args()
    generate(arguments.binary.resolve(), arguments.version, arguments.output.resolve())


if __name__ == "__main__":
    main()
