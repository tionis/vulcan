from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from io import BytesIO


SCRIPT_ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPT_ROOT))


def load_script(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPT_ROOT / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


package_script = load_script("package")
package_deb_script = load_script("package_deb")
manifest_script = load_script("manifest")
channels_script = load_script("channels")
update_channel_script = load_script("update_channel")
rolling_signer_script = load_script("sign_rolling_release")
stable_signer_script = load_script("sign_stable_release")
rolling_signer_installer = load_script("install_rolling_signer")


def read_ar(path: pathlib.Path) -> dict[str, bytes]:
    contents = path.read_bytes()
    if not contents.startswith(b"!<arch>\n"):
        raise ValueError("not an ar archive")
    offset = 8
    members = {}
    while offset < len(contents):
        header = contents[offset : offset + 60]
        if len(header) != 60 or header[58:60] != b"`\n":
            raise ValueError("invalid ar member header")
        name = header[:16].decode("ascii").strip().removesuffix("/")
        size = int(header[48:58].decode("ascii").strip())
        offset += 60
        members[name] = contents[offset : offset + size]
        offset += size + size % 2
    return members


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
        self.write_fake_elf("x86_64-unknown-linux-gnu")
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

    def package_deb(self, target: str) -> pathlib.Path:
        self.write_fake_elf(target)
        return package_deb_script.package(
            self.binary,
            self.assets,
            self.source,
            self.output,
            target,
            "1.2.3",
        )

    def write_fake_elf(self, target: str) -> None:
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = package_deb_script.TARGET_ELF_MACHINES[target].to_bytes(2, "little")
        self.binary.write_bytes(header)

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

    def test_debian_package_is_reproducible_and_has_expected_metadata(self) -> None:
        package = self.package_deb("x86_64-unknown-linux-gnu")
        first = hashlib.sha256(package.read_bytes()).hexdigest()
        package = self.package_deb("x86_64-unknown-linux-gnu")
        self.assertEqual(first, hashlib.sha256(package.read_bytes()).hexdigest())
        self.assertEqual(package.name, "vulcan_1.2.3-1_amd64.deb")

        members = read_ar(package)
        self.assertEqual(members["debian-binary"], b"2.0\n")
        with tarfile.open(fileobj=BytesIO(members["control.tar.gz"]), mode="r:gz") as control:
            self.assertEqual([member.name for member in control.getmembers()], ["./control"])
            metadata = control.extractfile("./control").read().decode("utf-8")
        self.assertIn("Package: vulcan\n", metadata)
        self.assertIn("Version: 1.2.3-1\n", metadata)
        self.assertIn("Architecture: amd64\n", metadata)
        self.assertIn("Depends: git, libc6, libgcc-s1\n", metadata)
        with tarfile.open(fileobj=BytesIO(members["data.tar.gz"]), mode="r:gz") as data:
            packaged = {member.name: member for member in data.getmembers()}
        self.assertEqual(packaged["./usr/bin/vulcan"].mode, 0o755)
        self.assertEqual(packaged["./usr"].mode, 0o755)
        self.assertIn("./usr/share/man/man1/vulcan.1.gz", packaged)
        self.assertIn("./usr/share/bash-completion/completions/vulcan", packaged)
        self.assertIn("./usr/share/doc/vulcan/INSTALL.md", packaged)
        self.assertIn("./usr/share/doc/vulcan/copyright", packaged)
        self.assertFalse(any("systemd" in path or "LaunchAgent" in path for path in packaged))
        if shutil.which("dpkg-deb"):
            inspected = subprocess.run(
                ["dpkg-deb", "--field", str(package), "Package", "Architecture"],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            ).stdout
            self.assertIn("Package: vulcan", inspected)
            self.assertIn("Architecture: amd64", inspected)

    def test_debian_versions_sort_prereleases_before_the_release(self) -> None:
        self.assertEqual(package_deb_script.debian_version("1.2.3"), "1.2.3-1")
        self.assertEqual(
            package_deb_script.debian_version("1.2.3-beta.1"),
            "1.2.3~beta.1-1",
        )
        with self.assertRaisesRegex(ValueError, "unsupported Debian target"):
            package_deb_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                "x86_64-apple-darwin",
                "1.2.3",
            )
        self.write_fake_elf("x86_64-unknown-linux-gnu")
        with self.assertRaisesRegex(ValueError, "does not match Debian target"):
            package_deb_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                "aarch64-unknown-linux-gnu",
                "1.2.3",
            )
        malformed = bytearray(self.binary.read_bytes())
        malformed[4] = 1
        self.binary.write_bytes(malformed)
        with self.assertRaisesRegex(ValueError, "not a 64-bit little-endian ELF"):
            package_deb_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                "x86_64-unknown-linux-gnu",
                "1.2.3",
            )

    def test_manifest_aggregation_verifies_archives(self) -> None:
        linux = self.package("x86_64-unknown-linux-gnu")
        windows = self.package("x86_64-pc-windows-msvc")
        debian = self.package_deb("x86_64-unknown-linux-gnu")
        expected = {
            ("archive", "x86_64-unknown-linux-gnu"),
            ("archive", "x86_64-pc-windows-msvc"),
            ("debian", "x86_64-unknown-linux-gnu"),
        }
        manifest, checksums = manifest_script.aggregate(self.output, "1.2.3", expected)
        data = json.loads(manifest.read_text(encoding="utf-8"))
        self.assertEqual(len(data["artifacts"]), 3)
        self.assertIn(linux.name, checksums.read_text(encoding="ascii"))
        self.assertIn(windows.name, checksums.read_text(encoding="ascii"))
        self.assertIn(debian.name, checksums.read_text(encoding="ascii"))

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
        self.assertEqual(installed.read_bytes(), self.binary.read_bytes())
        self.assertTrue(os.access(installed, os.X_OK))
        self.assertTrue((prefix / "share/man/man1/vulcan.1").is_file())

    def test_package_channel_metadata_uses_canonical_archives(self) -> None:
        for target in manifest_script.EXPECTED_TARGETS:
            self.package(target)
        for target in package_deb_script.TARGET_ARCHITECTURES:
            self.package_deb(target)
        manifest, _ = manifest_script.aggregate(
            self.output, "1.2.3", manifest_script.EXPECTED_ARTIFACTS
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
        self.assertNotIn(".deb", formula)

        rolling_manifest = json.loads(manifest.read_text(encoding="utf-8"))
        rolling_manifest["version"] = "1.2.4-dev.20260831.1.gaaaaaaaa"
        for artifact in rolling_manifest["artifacts"]:
            if artifact.get("kind", "archive") == "archive":
                artifact["top_level_directory"] = (
                    f'vulcan-{rolling_manifest["version"]}-{artifact["target"]}'
                )
        manifest.write_text(json.dumps(rolling_manifest), encoding="utf-8")
        update_channel = update_channel_script.generate(
            manifest,
            "main",
            "https://releases.example/main",
            "a" * 40,
            "2026-08-31T20:00:00+00:00",
            channel_output,
        )
        envelope = json.loads(update_channel.read_text(encoding="utf-8"))
        payload = json.loads(base64.b64decode(envelope["payload"]))
        self.assertEqual(envelope["signatures"], [])
        self.assertEqual(payload["channel"], "main")
        self.assertTrue(payload["prerelease"])
        self.assertEqual(len(payload["artifacts"]), 5)
        self.assertTrue(
            all(
                artifact["url"].startswith("https://releases.example/main/")
                for artifact in payload["artifacts"]
            )
        )
        with self.assertRaisesRegex(ValueError, "prerelease version"):
            update_channel_script.generate(
                manifest,
                "stable",
                "https://releases.example/stable",
                "a" * 40,
                "2026-08-31T20:00:00Z",
                channel_output,
            )

    @unittest.skipUnless(shutil.which("openssl"), "OpenSSL is required for signing test")
    def test_update_channel_signatures_cover_the_exact_payload_bytes(self) -> None:
        for target in manifest_script.EXPECTED_TARGETS:
            self.package(target)
        for target in package_deb_script.TARGET_ARCHITECTURES:
            self.package_deb(target)
        manifest, _ = manifest_script.aggregate(
            self.output, "1.2.3", manifest_script.EXPECTED_ARTIFACTS
        )
        key = self.root / "update-key.pem"
        public = self.root / "update-public.pem"
        subprocess.run(
            ["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(key)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        subprocess.run(
            ["openssl", "pkey", "-in", str(key), "-pubout", "-out", str(public)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        destination = update_channel_script.generate(
            manifest,
            "stable",
            "https://releases.example/v1.2.3",
            "b" * 40,
            "2026-08-31T20:00:00Z",
            self.root / "signed-channel",
            key,
            "release-2026",
        )
        envelope = json.loads(destination.read_text(encoding="utf-8"))
        signature_path = self.root / "signature.bin"
        payload_path = self.root / "payload.json"
        signature_path.write_bytes(
            base64.b64decode(envelope["signatures"][0]["signature"])
        )
        payload_path.write_bytes(base64.b64decode(envelope["payload"]))
        subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-rawin",
                "-pubin",
                "-inkey",
                str(public),
                "-sigfile",
                str(signature_path),
                "-in",
                str(payload_path),
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def rolling_release_fixture(self) -> tuple[dict, str, str]:
        source_commit = "a" * 40
        version = "1.2.4-dev.20260901.42.gaaaaaaaa"
        for target in manifest_script.EXPECTED_TARGETS:
            package_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                target,
                version,
            )
        for target in package_deb_script.TARGET_ARCHITECTURES:
            self.write_fake_elf(target)
            package_deb_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                target,
                version,
            )
        manifest, _ = manifest_script.aggregate(
            self.output, version, manifest_script.EXPECTED_ARTIFACTS
        )
        update_channel_script.generate(
            manifest,
            "main",
            "https://github.com/tionis/vulcan/releases/download/main",
            source_commit,
            "2026-09-01T12:00:00Z",
            self.output,
        )
        for path in list(self.output.glob("*.artifact.json")) + list(
            self.output.glob("*.sha256")
        ):
            path.unlink()
        labels = {}
        for path in list(self.output.glob("*.deb")):
            sanitized = path.with_name(path.name.replace("~", "."))
            labels[sanitized.name] = path.name
            path.rename(sanitized)
        release = {
            "id": 123,
            "tag_name": "main",
            "draft": False,
            "prerelease": True,
            "assets": [
                {
                    "id": index,
                    "name": path.name,
                    "label": labels.get(path.name, ""),
                    "size": path.stat().st_size,
                    "updated_at": "2026-09-01T12:01:00Z",
                }
                for index, path in enumerate(
                    sorted(self.output.iterdir()), start=1000
                )
                if path.is_file()
            ],
        }
        return release, source_commit, version

    def stable_release_fixture(self) -> tuple[dict, str, str, str]:
        source_commit = "b" * 40
        version = "1.2.3"
        tag = f"v{version}"
        for target in manifest_script.EXPECTED_TARGETS:
            package_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                target,
                version,
            )
        for target in package_deb_script.TARGET_ARCHITECTURES:
            self.write_fake_elf(target)
            package_deb_script.package(
                self.binary,
                self.assets,
                self.source,
                self.output,
                target,
                version,
            )
        manifest, _ = manifest_script.aggregate(
            self.output, version, manifest_script.EXPECTED_ARTIFACTS
        )
        update_channel_script.generate(
            manifest,
            "stable",
            f"https://github.com/tionis/vulcan/releases/download/{tag}",
            source_commit,
            "2026-09-02T12:00:00Z",
            self.output,
        )
        for path in list(self.output.glob("*.artifact.json")) + list(
            self.output.glob("*.sha256")
        ):
            path.unlink()
        release = {
            "id": 456,
            "tag_name": tag,
            "draft": False,
            "prerelease": False,
            "assets": [
                {
                    "id": index,
                    "name": path.name,
                    "label": "",
                    "size": path.stat().st_size,
                    "updated_at": "2026-09-02T12:01:00Z",
                }
                for index, path in enumerate(sorted(self.output.iterdir()), start=2000)
                if path.is_file()
            ],
        }
        return release, source_commit, version, tag

    def test_rolling_signer_validates_the_exact_published_artifact_set(self) -> None:
        release, source_commit, version = self.rolling_release_fixture()
        validated = rolling_signer_script.validate_downloaded_release(
            self.output,
            release,
            source_commit,
            "tionis/vulcan",
            source_commit,
        )
        self.assertEqual(validated.version, version)
        self.assertEqual(validated.source_commit, source_commit)

        archive = next(self.output.glob("*.tar.gz"))
        archive.write_bytes(archive.read_bytes() + b"tamper")
        with self.assertRaisesRegex(ValueError, "asset size mismatch|integrity mismatch"):
            rolling_signer_script.validate_downloaded_release(
                self.output,
                release,
                source_commit,
                "tionis/vulcan",
                source_commit,
            )

    def test_rolling_signer_requires_success_for_the_exact_commit(self) -> None:
        source_commit = "b" * 40
        successful = [
            {
                "headSha": source_commit,
                "headBranch": "main",
                "status": "completed",
                "conclusion": "success",
                "event": "push",
            }
        ]
        rolling_signer_script.validate_successful_runs(
            successful,
            source_commit,
            workflow="CI",
            required_event="push",
        )
        with self.assertRaisesRegex(ValueError, "no successful completed CI run"):
            rolling_signer_script.validate_successful_runs(
                successful,
                "c" * 40,
                workflow="CI",
                required_event="push",
            )

    def test_stable_signer_validates_an_exact_immutable_release(self) -> None:
        release, source_commit, version, tag = self.stable_release_fixture()
        validated = rolling_signer_script.validate_downloaded_release(
            self.output,
            release,
            source_commit,
            "tionis/vulcan",
            source_commit,
            tag=tag,
            channel="stable",
            prerelease=False,
            release_kind="stable",
        )
        self.assertEqual(validated.version, version)
        self.assertEqual(validated.source_commit, source_commit)

        with self.assertRaisesRegex(ValueError, "version and version tag"):
            rolling_signer_script.validate_downloaded_release(
                self.output,
                {**release, "tag_name": "v1.2.4"},
                source_commit,
                "tionis/vulcan",
                source_commit,
                tag="v1.2.4",
                channel="stable",
                prerelease=False,
                release_kind="stable",
            )

    def test_stable_signer_requires_explicit_tag_commit_approval(self) -> None:
        update_command = (
            SCRIPT_ROOT.parents[1] / "vulcan-cli/src/commands/update.rs"
        ).read_text(encoding="utf-8")
        self.assertIn(stable_signer_script.STABLE_KEY_ID, update_command)
        self.assertIn(stable_signer_script.STABLE_PUBLIC_KEY, update_command)

        with self.assertRaisesRegex(ValueError, "exactly v"):
            stable_signer_script.sign_stable_release(
                "tionis/vulcan",
                "main",
                "a" * 40,
                self.root / "unused.pem",
                stable_signer_script.STABLE_KEY_ID,
                True,
            )
        with self.assertRaisesRegex(ValueError, "full lowercase"):
            stable_signer_script.sign_stable_release(
                "tionis/vulcan",
                "v1.2.3",
                "abc123",
                self.root / "unused.pem",
                stable_signer_script.STABLE_KEY_ID,
                True,
            )

        successful = [
            {
                "headSha": "c" * 40,
                "headBranch": "v1.2.3",
                "status": "completed",
                "conclusion": "success",
                "event": "push",
            }
        ]
        rolling_signer_script.validate_successful_runs(
            successful,
            "c" * 40,
            workflow="stable release",
            required_event="push",
            expected_head_branch="v1.2.3",
        )
        with self.assertRaisesRegex(ValueError, "no successful completed"):
            rolling_signer_script.validate_successful_runs(
                successful,
                "c" * 40,
                workflow="stable release",
                required_event="push",
                expected_head_branch="main",
            )

    def test_rolling_signer_systemd_install_has_a_mutation_free_preview(self) -> None:
        key = self.root / "main key.pem"
        key.write_text("fixture only\n", encoding="utf-8")
        unit_directory = self.root / "systemd user"
        libexec_directory = self.root / "libexec"
        report = rolling_signer_installer.install(
            SCRIPT_ROOT,
            unit_directory,
            libexec_directory,
            "tionis/vulcan",
            key,
            True,
        )
        self.assertTrue(report["dry_run"])
        self.assertFalse(unit_directory.exists())
        self.assertFalse(libexec_directory.exists())
        service = rolling_signer_installer.render_service(
            pathlib.Path("/usr/bin/python3"),
            pathlib.Path('/tmp/path with "quotes"/sign.py'),
            "tionis/vulcan",
            key,
        )
        self.assertIn("ProtectHome=read-only", service)
        self.assertIn('\\"quotes\\"', service)
        self.assertIn("--signing-key", service)

    def test_release_workflows_publish_bounded_update_channels(self) -> None:
        workflows = SCRIPT_ROOT.parents[1] / ".github/workflows"
        stable = (workflows / "release.yml").read_text(encoding="utf-8")
        rolling = (workflows / "rolling-release.yml").read_text(encoding="utf-8")

        self.assertIn("scripts/release/update_channel.py", stable)
        self.assertIn("--channel stable", stable)
        self.assertNotIn("--signing-key", stable)
        self.assertIn('cron: "17 3 * * *"', rolling)
        self.assertIn("workflow_dispatch:", rolling)
        self.assertIn("--workflow CI", rolling)
        self.assertIn('if [[ "$previous_commit" == "$source_commit"', rolling)
        self.assertIn("-dev.", rolling)
        self.assertIn("VULCAN_UPDATE_CHANNEL: main", rolling)
        self.assertIn("--channel main", rolling)
        self.assertIn("retention-days: 1", rolling)
        self.assertIn("asset_label", rolling)
        self.assertIn("(.label // \"\")", rolling)
        self.assertIn('! -f "artifacts/$asset_label"', rolling)
        self.assertIn("key-holding machine", rolling)
        self.assertNotIn("--signing-key", rolling)
        self.assertNotIn("cargo test --workspace", rolling)
        self.assertLess(
            rolling.index("Publish rolling prerelease"),
            rolling.index("Prune superseded rolling assets"),
        )


if __name__ == "__main__":
    unittest.main()
