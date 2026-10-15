"""Checks for version gates, release checksums, and anonymous archive metadata."""

import hashlib
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import struct
import tempfile
import unittest


PROJECT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release", PROJECT / "scripts/release.py")
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class ReleaseTests(unittest.TestCase):
    def publication(self, version, state, tag=None, notes=None):
        with tempfile.TemporaryDirectory(prefix="sj-publish-") as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            (root / "bin").mkdir()
            (root / "Cargo.toml").write_text(f'[package]\nversion = "{version}"\n')
            if notes is not None:
                (root / "docs/releases").mkdir(parents=True)
                (root / f"docs/releases/{version}.md").write_text(notes)
            for name in ("release.py", "publish-release.sh"):
                shutil.copyfile(PROJECT / "scripts" / name, root / "scripts" / name)
            (root / "bin/python3").symlink_to(sys.executable)
            gh = root / "bin/gh"
            gh.write_text('''#!/bin/sh
if [ "$2" = view ]; then
  if [ "$SJ_TEST_STATE" = missing ]; then exit 1; fi
  printf '%s\\n' "$SJ_TEST_STATE"
else
  printf '%s\\n' "$*" >> "$SJ_TEST_CALLS"
fi
''')
            gh.chmod(0o755)
            tag = tag or "v" + version
            result = subprocess.run(
                ["bash", str(root / "scripts/publish-release.sh")],
                env=os.environ | {"PATH": str(root / "bin") + os.pathsep + os.environ["PATH"],
                                  "GITHUB_REF": "refs/tags/" + tag, "GITHUB_REF_NAME": tag,
                                  "GITHUB_REPOSITORY": "example/sj", "SJ_TEST_STATE": state,
                                  "SJ_TEST_CALLS": str(root / "calls")},
                capture_output=True, text=True,
            )
            calls = (root / "calls").read_text() if (root / "calls").exists() else ""
            return result, calls.splitlines()

    def test_publication_uploads_to_a_draft_before_publishing(self):
        for version, state in [("0.1.0", "missing"), ("0.1.0", "true"), ("0.2.0-rc.1", "missing")]:
            result, calls = self.publication(version, state)
            self.assertEqual(result.returncode, 0, result.stderr)
            if state == "missing":
                self.assertIn("--draft", calls[0])
                self.assertIn("--verify-tag", calls[0])
            self.assertIn("release upload", calls[-2])
            self.assertIn("dist/SHA256SUMS", calls[-2])
            self.assertIn("release edit", calls[-1])
            self.assertIn("--draft=false", calls[-1])
            if "-" in version:
                self.assertIn("--prerelease", calls[0])
                self.assertIn("--latest=false", calls[-1])

    def test_published_releases_and_mismatched_tags_never_upload_assets(self):
        result, calls = self.publication("0.1.0", "false")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        result, calls = self.publication("0.1.0", "missing", "v9.9.9")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_version_specific_release_notes_replace_generated_notes(self):
        result, calls = self.publication("0.2.0", "missing", notes="# sj 0.2.0\n\nConfigurable views and field filters.\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--notes-file docs/releases/0.2.0.md", calls[0])
        self.assertNotIn("--generate-notes", calls[0])
        result, calls = self.publication("0.2.1", "missing")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--generate-notes", calls[0])

    def test_static_elf_checks_reject_wrong_architecture_and_dynamic_linkage(self):
        def elf(machine, segment, dynamic=b""):
            header = bytearray(64)
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<HH", header, 16, 2, machine)
            struct.pack_into("<Q", header, 32, 64)
            struct.pack_into("<HH", header, 54, 56, 1)
            return header + struct.pack("<IIQQQQQQ", segment, 0, 120, 0, 0, len(dynamic), 0, 0) + dynamic

        with tempfile.TemporaryDirectory(prefix="sj-elf-") as temporary:
            binary = Path(temporary) / "sj"
            for target, machine in [(RELEASE.TARGETS[0], 62), (RELEASE.TARGETS[1], 183)]:
                binary.write_bytes(elf(machine, 1))
                RELEASE.verify_static_elf(binary, target)
                for invalid in [elf(0, 1), elf(machine, 3), elf(machine, 2, struct.pack("<qQ", 1, 0)), b"invalid"]:
                    binary.write_bytes(invalid)
                    with self.assertRaises(ValueError):
                        RELEASE.verify_static_elf(binary, target)

    def test_tags_must_match_the_package_version(self):
        RELEASE.validate_ref("0.1.0", "refs/tags/v0.1.0", "v0.1.0")
        RELEASE.validate_ref("0.1.0-rc.1", "refs/tags/v0.1.0-rc.1", "v0.1.0-rc.1")
        RELEASE.validate_ref("0.1.0", "refs/heads/main", "main")
        for tag in ("v9.9.9", "vnext", "0.1.0"):
            with self.assertRaises(ValueError):
                RELEASE.validate_ref("0.1.0", "refs/tags/" + tag, tag)

    def test_archives_are_reproducible_and_have_portable_metadata(self):
        with tempfile.TemporaryDirectory(prefix="sj-release-") as temporary:
            root = Path(temporary)
            (root / "Cargo.toml").write_text('[package]\nversion = "0.1.0"\n')
            (root / "docs/assets").mkdir(parents=True)
            for name in ("README.md", "README.zh-CN.md", "docs/assets/terminal-preview.svg"):
                (root / name).write_text("example")
            binary = root / "binary"
            binary.write_bytes(b"example binary")
            for target in RELEASE.TARGETS:
                first = RELEASE.package(root, binary, target, root / "first", 1)
                second = RELEASE.package(root, binary, target, root / "second", 1)
                self.assertEqual(first.read_bytes(), second.read_bytes())
                digest = hashlib.sha256(first.read_bytes()).hexdigest()
                self.assertEqual(first.with_name(first.name + ".sha256").read_text(), f"{digest}  {first.name}\n")
                with tarfile.open(first) as bundle:
                    self.assertEqual(bundle.extractfile("sj").read(), binary.read_bytes())
                    for member in bundle.getmembers():
                        self.assertEqual((member.uid, member.gid, member.uname, member.gname, member.mtime), (0, 0, "", "", 1))
                        self.assertEqual(member.mode, 0o755 if member.name == "sj" else 0o644)
                        self.assertFalse(member.name.startswith("/"))

    def test_unsupported_architecture_does_not_create_an_archive(self):
        with tempfile.TemporaryDirectory(prefix="sj-release-") as temporary:
            root = Path(temporary)
            with self.assertRaises(ValueError):
                RELEASE.package(root, root / "binary", "unsupported-target", root / "dist", 1)
            self.assertFalse((root / "dist").exists())


if __name__ == "__main__":
    unittest.main()
