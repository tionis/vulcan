from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest
import zipfile


SCRIPT_ROOT = pathlib.Path(__file__).resolve().parents[1]


def load_script(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPT_ROOT / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


package_script = load_script("package")
manifest_script = load_script("manifest")
channels_script = load_script("channels")


class ReleasePackagingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.source = self.root / "source"
        self.assets = self.root / "assets"
        self.output = self.root / "output"
        (self.source / "docs").mkdir(parents=True)
        (self.assets / "completions").mkdir(parents=True)
        for name in ("README.md", "LICENSE-MIT", "LICENSE-APACHE"):
            (self.source / name).write_text(name + "\n", encoding="utf-8")
        (self.source / "docs/installation.md").write_text("Install\n", encoding="utf-8")
        for name in package_script.ASSET_FILES:
            path = self.assets / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name + "\n", encoding="utf-8")
        self.binary = self.root / "vulcan"
        self.binary.write_bytes(b"test-binary")
        os.chmod(self.binary, 0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def package(self, target: str) -> pathlib.Path:
        return package_script.package(
            self.binary,
            self.assets,
            self.source,
            self.output,
            target,
            "1.2.3",
        )

    def test_tar_archive_is_reproducible_and_has_stable_layout(self) -> None:
        archive = self.package("x86_64-unknown-linux-gnu")
        first = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive = self.package("x86_64-unknown-linux-gnu")
        self.assertEqual(first, hashlib.sha256(archive.read_bytes()).hexdigest())
        with tarfile.open(archive, "r:gz") as packaged:
            members = {member.name: member for member in packaged.getmembers()}
        root = "vulcan-1.2.3-x86_64-unknown-linux-gnu"
        self.assertIn(f"{root}/vulcan", members)
        self.assertIn(f"{root}/completions/vulcan.fish", members)
        self.assertIn(f"{root}/vulcan.1", members)
        self.assertIn(f"{root}/INSTALL.md", members)
        self.assertEqual(members[f"{root}/vulcan"].mode, 0o755)

    def test_windows_archive_uses_zip_and_executable_suffix(self) -> None:
        archive = self.package("x86_64-pc-windows-msvc")
        self.assertEqual(archive.suffix, ".zip")
        with zipfile.ZipFile(archive) as packaged:
            names = packaged.namelist()
        self.assertIn("vulcan-1.2.3-x86_64-pc-windows-msvc/vulcan.exe", names)
        self.assertNotIn("vulcan-1.2.3-x86_64-pc-windows-msvc/vulcan", names)

    def test_manifest_aggregation_verifies_archives(self) -> None:
        linux = self.package("x86_64-unknown-linux-gnu")
        windows = self.package("x86_64-pc-windows-msvc")
        manifest, checksums = manifest_script.aggregate(self.output, "1.2.3")
        data = json.loads(manifest.read_text(encoding="utf-8"))
        self.assertEqual(len(data["artifacts"]), 2)
        self.assertIn(linux.name, checksums.read_text(encoding="ascii"))
        self.assertIn(windows.name, checksums.read_text(encoding="ascii"))

        linux.write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            manifest_script.aggregate(self.output, "1.2.3")

    def test_posix_installer_verifies_and_installs_canonical_archive(self) -> None:
        self.package("x86_64-unknown-linux-gnu")
        manifest_script.aggregate(self.output, "1.2.3")
        prefix = self.root / "prefix"
        subprocess.run(
            [
                "sh",
                str(SCRIPT_ROOT.parent / "install.sh"),
                "--version",
                "1.2.3",
                "--prefix",
                str(prefix),
                "--base-url",
                self.output.as_uri(),
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        installed = prefix / "bin/vulcan"
        self.assertEqual(installed.read_bytes(), b"test-binary")
        self.assertTrue(os.access(installed, os.X_OK))
        self.assertTrue((prefix / "share/man/man1/vulcan.1").is_file())

    def test_package_channel_metadata_uses_canonical_archives(self) -> None:
        for target in manifest_script.EXPECTED_TARGETS:
            self.package(target)
        manifest, _ = manifest_script.aggregate(
            self.output, "1.2.3", manifest_script.EXPECTED_TARGETS
        )
        channel_output = self.root / "channels"
        channels_script.generate(manifest, "https://releases.example/v1.2.3", channel_output)
        formula = (channel_output / "vulcan.rb").read_text(encoding="utf-8")
        self.assertIn('run [opt_bin/"vulcan", "daemon", "start"]', formula)
        self.assertIn("aarch64-apple-darwin.tar.gz", formula)
        self.assertIn("x86_64-unknown-linux-gnu.tar.gz", formula)
        winget = (channel_output / "Tionis.Vulcan.installer.yaml").read_text(
            encoding="utf-8"
        )
        self.assertIn("InstallerType: portable", winget)
        self.assertIn("x86_64-pc-windows-msvc.zip", winget)
        self.assertIn("PortableCommandAlias: vulcan", winget)


if __name__ == "__main__":
    unittest.main()
