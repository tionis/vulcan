#!/usr/bin/env python3
"""Render package-manager metadata from the canonical release manifest."""

from __future__ import annotations

import argparse
import json
import pathlib


def records_by_target(manifest: dict) -> dict[str, dict]:
    return {
        record["target"]: record
        for record in manifest["artifacts"]
        if record.get("kind", "archive") == "archive"
    }


def render_homebrew(manifest: dict, base_url: str) -> str:
    records = records_by_target(manifest)

    def source(target: str) -> str:
        record = records[target]
        return (
            f'      url "{base_url}/{record["name"]}"\n'
            f'      sha256 "{record["sha256"]}"\n'
        )

    return (
        "class Vulcan < Formula\n"
        '  desc "Local-first Markdown information hub"\n'
        '  homepage "https://github.com/tionis/vulcan"\n'
        f'  version "{manifest["version"]}"\n\n'
        "  on_macos do\n"
        "    if Hardware::CPU.arm?\n"
        + source("aarch64-apple-darwin")
        + "    else\n"
        + source("x86_64-apple-darwin")
        + "    end\n"
        "  end\n\n"
        "  on_linux do\n"
        "    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?\n"
        + source("aarch64-unknown-linux-gnu")
        + "    else\n"
        + source("x86_64-unknown-linux-gnu")
        + "    end\n"
        "  end\n\n"
        "  def install\n"
        '    bin.install "vulcan"\n'
        '    man1.install "vulcan.1"\n'
        '    bash_completion.install "completions/vulcan.bash" => "vulcan"\n'
        '    fish_completion.install "completions/vulcan.fish"\n'
        '    zsh_completion.install "completions/_vulcan"\n'
        "  end\n\n"
        "  service do\n"
        '    run [opt_bin/"vulcan", "daemon", "start"]\n'
        "    keep_alive crashed: true\n"
        "    process_type :background\n"
        '    log_path var/"log/vulcan-daemon.log"\n'
        '    error_log_path var/"log/vulcan-daemon.error.log"\n'
        "  end\n\n"
        "  test do\n"
        '    assert_match "vulcan #{version}", shell_output("#{bin}/vulcan --version")\n'
        "  end\n"
        "end\n"
    )


def render_winget(manifest: dict, base_url: str) -> dict[str, str]:
    record = records_by_target(manifest)["x86_64-pc-windows-msvc"]
    version = manifest["version"]
    installer = f"""PackageIdentifier: Tionis.Vulcan
PackageVersion: {version}
InstallerType: portable
Commands:
  - vulcan
Installers:
  - Architecture: x64
    InstallerUrl: {base_url}/{record['name']}
    InstallerSha256: {record['sha256'].upper()}
    NestedInstallerType: portable
    NestedInstallerFiles:
      - RelativeFilePath: vulcan-{version}-x86_64-pc-windows-msvc\\vulcan.exe
        PortableCommandAlias: vulcan
ManifestType: installer
ManifestVersion: 1.9.0
"""
    default_locale = f"""PackageIdentifier: Tionis.Vulcan
PackageVersion: {version}
PackageLocale: en-US
Publisher: tionis
PackageName: Vulcan
License: MIT OR Apache-2.0
ShortDescription: Local-first Markdown information hub
PackageUrl: https://github.com/tionis/vulcan
ManifestType: defaultLocale
ManifestVersion: 1.9.0
"""
    version_manifest = f"""PackageIdentifier: Tionis.Vulcan
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.9.0
"""
    return {
        "Tionis.Vulcan.installer.yaml": installer,
        "Tionis.Vulcan.locale.en-US.yaml": default_locale,
        "Tionis.Vulcan.yaml": version_manifest,
    }


def generate(manifest_path: pathlib.Path, base_url: str, output: pathlib.Path) -> None:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    output.mkdir(parents=True, exist_ok=True)
    (output / "vulcan.rb").write_text(
        render_homebrew(manifest, base_url), encoding="utf-8", newline="\n"
    )
    for name, contents in render_winget(manifest, base_url).items():
        (output / name).write_text(contents, encoding="utf-8", newline="\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True, type=pathlib.Path)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args()
    generate(
        arguments.manifest.resolve(),
        arguments.base_url.rstrip("/"),
        arguments.output.resolve(),
    )


if __name__ == "__main__":
    main()
