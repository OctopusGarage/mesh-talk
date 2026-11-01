#!/usr/bin/env python3
"""Exercise release payload inspection against real NSIS plugin installers.

Requires makensis and the release 7-Zip toolchain. Compiles and inspects packages;
never executes an installer or its application payload.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from delta_assets import equivalent, nsis_files


class NativeNsisTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compiler = shutil.which("makensis")
        if not cls.compiler:
            raise RuntimeError("makensis is required for native release inspection tests")
        cls.temporary = tempfile.TemporaryDirectory(prefix="mesh-native-nsis-")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.root = Path(cls.temporary.name)
        cls.canonical = cls.build("canonical", compressed=True)
        cls.raw = cls.build("raw")

    @classmethod
    def build(cls, name, compressed=False, application=b"MZ__TAURI_BUNDLE_TYPE_VAR_NSS", resource=b"owned resource"):
        root = cls.root / name
        root.mkdir()
        (root / "mesh-talk.exe").write_bytes(application)
        (root / "config.json").write_bytes(resource)
        compression = "SetCompressor /SOLID lzma" if compressed else "SetCompress off"
        (root / "installer.nsi").write_text(r'''
Unicode true
Name "Mesh Talk native inspection fixture"
OutFile "installer.exe"
InstallDir "$TEMP\MeshTalkInspectionFixture"
''' + compression + r'''
Section
 SetOutPath "$INSTDIR"
 ; A real plugin call emits the four-parameter EW_SETFLAG instruction that
 ; 7-Zip 26.00 misdecodes as BadCmd=13, losing the final member's size.
 System::Call 'kernel32::GetTickCount() i .r0'
 File "mesh-talk.exe"
 SetOutPath "$INSTDIR\resources"
 File "config.json"
 WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd
Section "Uninstall"
 Delete "$INSTDIR\mesh-talk.exe"
SectionEnd
''', encoding="utf-8")
        subprocess.run([cls.compiler, "-V2", "installer.nsi"], cwd=root, check=True, capture_output=True, timeout=60)
        return root / "installer.exe"

    def test_real_plugin_installer_preserves_complete_native_payload(self):
        equivalent(self.canonical, self.raw, "windows")
        members = nsis_files(self.canonical)
        self.assertEqual(set(members), {"mesh-talk.exe", "resources/config.json", "$PLUGINSDIR/System.dll"})
        self.assertEqual(members["mesh-talk.exe"], b"MZ__TAURI_BUNDLE_TYPE_VAR_NSS")
        self.assertEqual(members["resources/config.json"], b"owned resource")
        self.assertTrue(members["$PLUGINSDIR/System.dll"].startswith(b"MZ"))

    def test_changed_application_is_rejected(self):
        changed = self.build("changed-application", application=b"MZ__TAURI_BUNDLE_TYPE_VAR_NSS changed")
        with self.assertRaisesRegex(ValueError, "application files differ"):
            equivalent(self.canonical, changed, "windows")

    def test_changed_resource_is_rejected(self):
        changed = self.build("changed-resource", resource=b"substituted resource")
        with self.assertRaisesRegex(ValueError, "application files differ"):
            equivalent(self.canonical, changed, "windows")


if __name__ == "__main__":
    unittest.main()
