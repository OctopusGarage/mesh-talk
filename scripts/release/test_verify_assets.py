"""Exercise real archives; cryptographic signatures are checked separately by CI."""

import hashlib
import json
import plistlib
import tempfile
import sys
import unittest
import zipfile
from pathlib import Path

from verify_assets import elf_fingerprint, native_metadata, normalize_bundle_markers, verify_directory


class ReleaseAssetsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="mesh-talk-assets-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.entries = {}
        for platform, extensions in {
            "macos_arm64": ["dmg"], "macos_x86_64": ["dmg"],
            "windows_x86_64": ["exe", "msi"],
            "linux_x86_64": ["deb", "rpm", "AppImage"],
        }.items():
            entries = {f"release/installer 0.1.5.{ext}": b"installer bytes" for ext in extensions}
            if platform.startswith("macos"):
                app = "release/mesh-talk.app/Contents/"
                entries[app + "Info.plist"] = plistlib.dumps({"CFBundleShortVersionString": "0.1.5", "CFBundleExecutable": "mesh-talk"})
                cpu = 0x0100000C if platform.endswith("arm64") else 0x01000007
                entries[app + "MacOS/mesh-talk"] = b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little") + b"\0" * 24
            self.entries[platform] = entries
            self.write_archive(platform)
            (self.root / (self.name(platform) + ".bundle")).write_text('{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json"}')
        (self.root / "mesh-talk.cdx.json").write_text(json.dumps({"bomFormat": "CycloneDX", "components": [{"name": "mesh-talk"}]}))

    @staticmethod
    def name(platform):
        return f"mesh-talk_v0.1.5_{platform}.zip"

    def write_archive(self, platform, checksums=None):
        entries = self.entries[platform]
        if checksums is None:
            checksums = "".join(f"{hashlib.sha256(data).hexdigest()}  ./{name.removeprefix('release/')}\n" for name, data in entries.items() if name.count("/") == 1)
        with zipfile.ZipFile(self.root / self.name(platform), "w") as archive:
            for name, data in entries.items():
                archive.writestr(name, data)
            archive.writestr("release/SHA256SUMS", checksums)

    def verify(self):
        # Structural tests intentionally use non-executable installer fixtures.
        # The CLI's production path always validates native installer metadata.
        return verify_directory(self.root, "v0.1.5", "0.1.5", check_installers=False)

    def test_complete_release(self):
        self.assertEqual(len(self.verify()), 4)

    def test_missing_platform(self):
        (self.root / self.name("windows_x86_64")).unlink()
        with self.assertRaisesRegex(ValueError, "asset set"):
            self.verify()

    def test_missing_bundle(self):
        (self.root / (self.name("linux_x86_64") + ".bundle")).unlink()
        with self.assertRaisesRegex(ValueError, "asset set"):
            self.verify()

    def test_extra_asset(self):
        (self.root / "old.zip").write_bytes(b"stale")
        with self.assertRaisesRegex(ValueError, "asset set"):
            self.verify()

    def test_blank_checksums(self):
        self.write_archive("windows_x86_64", "\n\n")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()

    def test_partial_checksums(self):
        self.write_archive("windows_x86_64", f"{hashlib.sha256(b'installer bytes').hexdigest()}  ./installer 0.1.5.exe\n")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()

    def test_wrong_hash(self):
        self.write_archive("macos_arm64", f"{'0' * 64}  ./installer 0.1.5.dmg\n")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()

    def test_duplicate_checksum(self):
        line = f"{hashlib.sha256(b'installer bytes').hexdigest()}  ./installer 0.1.5.dmg\n"
        self.write_archive("macos_arm64", line * 2)
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()

    def test_binary_mode_checksums(self):
        self.write_archive("macos_arm64", f"{hashlib.sha256(b'installer bytes').hexdigest()} *./installer 0.1.5.dmg\r\n")
        self.verify()

    def test_missing_installer_format(self):
        self.entries["linux_x86_64"].pop("release/installer 0.1.5.rpm")
        self.write_archive("linux_x86_64")
        with self.assertRaisesRegex(ValueError, "installer"):
            self.verify()

    def test_path_traversal(self):
        self.entries["macos_arm64"]["release/../outside"] = b"bad"
        self.write_archive("macos_arm64")
        with self.assertRaisesRegex(ValueError, "archive path"):
            self.verify()

    def test_duplicate_archive_member(self):
        with zipfile.ZipFile(self.root / self.name("windows_x86_64"), "a") as archive:
            archive.writestr("release/SHA256SUMS", "bad")
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.verify()

    def test_installer_symlink(self):
        path = self.root / self.name("windows_x86_64")
        with zipfile.ZipFile(path, "a") as archive:
            member = zipfile.ZipInfo("release/link.exe")
            member.create_system = 3
            member.external_attr = 0o120777 << 16
            archive.writestr(member, "installer 0.1.5.exe")
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.verify()

    def test_wrong_macos_version(self):
        self.entries["macos_arm64"]["release/mesh-talk.app/Contents/Info.plist"] = plistlib.dumps({"CFBundleShortVersionString": "0.1.4", "CFBundleExecutable": "mesh-talk"})
        self.write_archive("macos_arm64")
        with self.assertRaisesRegex(ValueError, "version"):
            self.verify()

    def test_wrong_macos_architecture(self):
        self.entries["macos_arm64"]["release/mesh-talk.app/Contents/MacOS/mesh-talk"] = b"\xcf\xfa\xed\xfe" + (0x01000007).to_bytes(4, "little")
        self.write_archive("macos_arm64")
        with self.assertRaisesRegex(ValueError, "architecture"):
            self.verify()

    def test_empty_sbom(self):
        (self.root / "mesh-talk.cdx.json").write_text('{"bomFormat":"CycloneDX","components":[]}')
        with self.assertRaisesRegex(ValueError, "SBOM"):
            self.verify()

    def test_cli_cannot_disable_metadata_validation(self):
        import subprocess
        result = subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("verify_assets.py")), str(self.root), "--tag", "v0.1.5", "--version", "0.1.5"], capture_output=True)
        self.assertNotEqual(result.returncode, 0)

    def test_nsis_bootstrapper_can_be_x86_for_an_x64_application(self):
        from unittest.mock import patch
        for machine in (b"\x4c\x01", b"\x64\x86"):
            header = bytearray(72)
            header[:2] = b"MZ"
            header[0x3C:0x40] = (64).to_bytes(4, "little")
            header[64:70] = b"PE\0\0" + machine
            path = self.root / "bootstrapper.exe"
            path.write_bytes(header)
            metadata = json.dumps([{"ProductVersion": "0.1.5.0", "FileVersion": "0.1.5.0"}])
            with patch("verify_assets.subprocess.check_output", return_value=metadata):
                native_metadata(path, "exe", "0.1.5")

    def test_non_dmg_file_is_rejected(self):
        path = self.root / "fake.dmg"
        path.write_bytes(b"not a disk image")
        with self.assertRaisesRegex(ValueError, "DMG"):
            native_metadata(path, "dmg", "0.1.5")

    def test_wrong_native_package_metadata_is_rejected(self):
        from unittest.mock import patch
        path = self.root / "fixture"
        path.write_bytes(b"installer")
        for extension, output in [("deb", "0.1.4"), ("rpm", "0.1.4\nx86_64"), ("msi", "ProductVersion\t0.1.4")]:
            with self.subTest(extension=extension), patch("verify_assets.subprocess.check_output", return_value=output):
                with self.assertRaisesRegex(ValueError, "version"):
                    native_metadata(path, extension, "0.1.5")

    def test_wrong_native_package_architecture_is_rejected(self):
        from unittest.mock import patch
        path = self.root / "fixture"
        path.write_bytes(b"installer")
        for extension, outputs in [("deb", ["0.1.5", "arm64"]), ("rpm", ["0.1.5\naarch64"]), ("msi", ["ProductVersion\t0.1.5", "Template: Intel;1033"])]:
            with self.subTest(extension=extension), patch("verify_assets.subprocess.check_output", side_effect=outputs):
                with self.assertRaisesRegex(ValueError, "architecture"):
                    native_metadata(path, extension, "0.1.5")

    def test_dmg_embedded_version_and_architecture_are_checked(self):
        from unittest.mock import patch
        path = self.root / "image.dmg"
        trailer = bytearray(512)
        trailer[:4] = b"koly"
        path.write_bytes(trailer)
        with patch("verify_assets.archive_member", return_value=plistlib.dumps({"CFBundleShortVersionString": "0.1.4", "CFBundleExecutable": "mesh-talk"})):
            with self.assertRaisesRegex(ValueError, "version"):
                native_metadata(path, "dmg", "0.1.5", "macos_arm64")
        with patch("verify_assets.archive_member", side_effect=[plistlib.dumps({"CFBundleShortVersionString": "0.1.5", "CFBundleExecutable": "mesh-talk"}), b"\xcf\xfa\xed\xfe" + (0x01000007).to_bytes(4, "little")]):
            with self.assertRaisesRegex(ValueError, "architecture"):
                native_metadata(path, "dmg", "0.1.5", "macos_arm64")

    def test_invalid_appimage_is_rejected(self):
        path = self.root / "fake.AppImage"
        path.write_bytes(b"not an AppImage")
        with self.assertRaisesRegex(ValueError, "architecture"):
            native_metadata(path, "AppImage", "0.1.5")

    def test_appimage_uses_native_squashfs_without_executing_runtime(self):
        import struct
        from unittest.mock import patch
        runtime = bytearray(128)
        runtime[:6] = b"\x7fELF\x02\x01"
        runtime[8:11] = b"AI\x02"
        runtime[18:20] = b"\x3e\0"
        struct.pack_into("<Q", runtime, 40, 64)
        struct.pack_into("<HH", runtime, 58, 64, 1)
        # Runtime may end after its section table; mirror the official runtime.
        struct.pack_into("<QQ", runtime, 64 + 24, 128, 16)
        path = self.root / "fixture.AppImage"
        path.write_bytes(runtime + b"runtime padding!" + b"hsqs" + bytes(92))
        executable = self.elf_fixture()
        with patch("verify_assets.subprocess.check_output", return_value=executable) as command:
            self.assertEqual(native_metadata(path, "AppImage", "0.1.5"), {"elf": elf_fingerprint(executable)})
            command.assert_called_once_with(["unsquashfs", "-cat", "-no-wildcards", "-o", "144", str(path), "usr/bin/mesh-talk"], timeout=60)
        for bad in [runtime[:64], runtime + b"not squashfs" + bytes(96)]:
            path.write_bytes(bad)
            with patch("verify_assets.subprocess.check_output") as command:
                with self.assertRaisesRegex(ValueError, "AppImage"):
                    native_metadata(path, "AppImage", "0.1.5")
                command.assert_not_called()

    def test_bundle_normalization_only_ignores_tauri_package_markers(self):
        prefix = b"version=0.1.5;"
        self.assertEqual(normalize_bundle_markers(prefix + b"__TAURI_BUNDLE_TYPE_VAR_DEB"), normalize_bundle_markers(prefix + b"__TAURI_BUNDLE_TYPE_VAR_APP"))
        self.assertNotEqual(normalize_bundle_markers(b"version=0.1.4;__TAURI_BUNDLE_TYPE_VAR_APP"), normalize_bundle_markers(prefix + b"__TAURI_BUNDLE_TYPE_VAR_DEB"))

    @staticmethod
    def elf_fixture(text=b"code", constants=b"version=0.1.5;__TAURI_BUNDLE_TYPE_VAR_DEB"):
        import struct
        names = b"\0.text\0.rodata\0.shstrtab\0"
        binary = bytearray(320)
        binary[:6] = b"\x7fELF\x02\x01"
        binary[18:20] = b"\x3e\0"
        struct.pack_into("<Q", binary, 40, 64)
        struct.pack_into("<HHH", binary, 58, 64, 4, 3)
        for index, name, data in [(1, 1, text), (2, 7, constants), (3, 15, names)]:
            offset = len(binary)
            binary.extend(data)
            struct.pack_into("<I", binary, 64 + index * 64, name)
            struct.pack_into("<QQ", binary, 64 + index * 64 + 24, offset, len(data))
        return bytes(binary)

    def test_elf_fingerprint_binds_code_and_constants(self):
        original = elf_fingerprint(self.elf_fixture())
        self.assertEqual(original, elf_fingerprint(self.elf_fixture(constants=b"version=0.1.5;__TAURI_BUNDLE_TYPE_VAR_APP")))
        self.assertNotEqual(original, elf_fingerprint(self.elf_fixture(text=b"changed code")))
        self.assertNotEqual(original, elf_fingerprint(self.elf_fixture(constants=b"version=0.1.4;__TAURI_BUNDLE_TYPE_VAR_APP")))
        with self.assertRaisesRegex(ValueError, "bounds"):
            elf_fingerprint(self.elf_fixture()[:100])

    def test_native_packages_must_match_companion_applications(self):
        from unittest.mock import patch
        original = elf_fingerprint(self.elf_fixture())
        def metadata(path, extension, version, platform):
            if extension == "dmg":
                binary = self.entries[platform]["release/mesh-talk.app/Contents/MacOS/mesh-talk"]
                return {"binary": hashlib.sha256(binary).hexdigest()}
            return {"elf": original}
        with patch("verify_assets.native_metadata", side_effect=metadata):
            verify_directory(self.root, "v0.1.5", "0.1.5")
        def wrong_dmg(path, extension, version, platform):
            return {"binary": "0" * 64} if extension == "dmg" else metadata(path, extension, version, platform)
        with patch("verify_assets.native_metadata", side_effect=wrong_dmg):
            with self.assertRaisesRegex(ValueError, "DMG/application"):
                verify_directory(self.root, "v0.1.5", "0.1.5")
        for binary in [self.elf_fixture(text=b"changed"), self.elf_fixture(constants=b"different version")]:
            def wrong_appimage(path, extension, version, platform):
                return {"elf": elf_fingerprint(binary)} if extension == "AppImage" else metadata(path, extension, version, platform)
            with patch("verify_assets.native_metadata", side_effect=wrong_appimage):
                with self.assertRaisesRegex(ValueError, "AppImage/versioned Debian"):
                    verify_directory(self.root, "v0.1.5", "0.1.5")


if __name__ == "__main__":
    unittest.main()
