import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("apply_gpui_patches", ROOT / "scripts/apply-gpui-patches.py")
PATCHER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PATCHER)


class PatchApplierTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root / "upstream"
        self.source.mkdir(mode=0o700)
        self.run_git("init", "-q")
        self.run_git("config", "user.email", "patch-test@example.invalid")
        self.run_git("config", "user.name", "Patch Test")
        (self.source / "src").mkdir()
        (self.source / "src/a.rs").write_text("pub fn a() -> u8 { 1 }\n")
        (self.source / "src/b.rs").write_text("pub fn b() -> u8 { 2 }\n")
        self.run_git("add", "src/a.rs", "src/b.rs")
        self.run_git("commit", "-q", "-m", "upstream")
        self.commit = self.run_git("rev-parse", "HEAD").stdout.strip()
        self.before = {
            "src/a.rs": (self.source / "src/a.rs").read_bytes(),
            "src/b.rs": (self.source / "src/b.rs").read_bytes(),
        }
        (self.source / "src/a.rs").write_text("pub fn a() -> u8 { 10 }\n")
        (self.source / "src/b.rs").write_text("pub fn b() -> u8 { 20 }\n")
        self.patch = self.run_git("diff", "--", "src/a.rs", "src/b.rs").stdout.encode()
        self.after = {
            "src/a.rs": (self.source / "src/a.rs").read_bytes(),
            "src/b.rs": (self.source / "src/b.rs").read_bytes(),
        }
        self.run_git("checkout", "--", ".")
        (self.root / "patches").mkdir()
        (self.root / "patches/core.patch").write_bytes(self.patch)
        self.manifest_path = self.root / "manifest.json"
        self.manifest = {
            "schema": 1,
            "upstream_commit": self.commit,
            "profile": "test-profile-v1",
            "patches": [{"path": "patches/core.patch",
                         "sha256": hashlib.sha256(self.patch).hexdigest()}],
            "files": [
                {"path": name, "before_sha256": hashlib.sha256(self.before[name]).hexdigest(),
                 "after_sha256": hashlib.sha256(self.after[name]).hexdigest()}
                for name in sorted(self.before)
            ],
        }
        self.write_manifest()

    def tearDown(self):
        self.temp.cleanup()

    def run_git(self, *args):
        return subprocess.run(["git", "-C", str(self.source), *args], check=True,
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True)

    def write_manifest(self):
        self.manifest_path.write_text(json.dumps(self.manifest))

    def apply(self):
        return PATCHER.apply(self.source, self.manifest_path)

    def make_archive_source(self):
        outer = self.root / "outer-git"
        outer.mkdir(mode=0o700)
        subprocess.run(["git", "-C", str(outer), "init", "-q"], check=True)
        source = outer / ".upstream" / f"zed-{self.commit}"
        (source / "src").mkdir(parents=True)
        for name, data in self.before.items():
            (source / name).write_bytes(data)
        archive = self.root / "upstream.tar.gz"
        self.write_archive(archive, list(self.before))
        return outer, source, archive

    def write_archive(self, archive, names):
        with tarfile.open(archive, "w:gz") as bundle:
            root_info = tarfile.TarInfo(f"zed-{self.commit}")
            root_info.type = tarfile.DIRTYPE
            root_info.mode = 0o755
            bundle.addfile(root_info)
            for name in names:
                data = self.before[name]
                info = tarfile.TarInfo(f"zed-{self.commit}/{name}")
                info.size = len(data)
                info.mode = 0o644
                bundle.addfile(info, io.BytesIO(data))

    def archive_manifest(self, archive):
        manifest = json.loads(json.dumps(self.manifest))
        manifest["upstream_archive_sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.manifest_path.write_text(json.dumps(manifest))
        return manifest

    def test_applies_two_files_then_is_idempotent(self):
        first = self.apply()
        self.assertEqual(first["status"], "applied")
        for name, expected in self.after.items():
            self.assertEqual((self.source / name).read_bytes(), expected)
        second = self.apply()
        self.assertEqual(second["status"], "already-applied")

    def test_patch_hash_mismatch_fails_before_mutation(self):
        self.manifest["patches"][0]["sha256"] = "0" * 64
        self.write_manifest()
        with self.assertRaisesRegex(PATCHER.PatchError, "patch hash mismatch"):
            self.apply()
        self.assertEqual((self.source / "src/a.rs").read_bytes(), self.before["src/a.rs"])

    def test_traversal_and_symlink_patch_paths_fail_closed(self):
        self.manifest["files"][0]["path"] = "../outside.rs"
        self.write_manifest()
        with self.assertRaisesRegex(PATCHER.PatchError, "cannot traverse"):
            self.apply()

    def test_symlink_patch_file_is_rejected(self):
        self.manifest["patches"][0]["path"] = "patches/link.patch"
        outside = self.root / "outside.patch"
        outside.write_bytes(self.patch)
        (self.root / "patches/link.patch").symlink_to(outside)
        self.write_manifest()
        with self.assertRaisesRegex(PATCHER.PatchError, "symlink"):
            self.apply()

    def test_unknown_patch_target_fails_before_mutation(self):
        unknown = self.patch.replace(b"src/a.rs", b"src/unknown.rs")
        patch_path = self.root / "patches/core.patch"
        patch_path.write_bytes(unknown)
        self.manifest["patches"][0]["sha256"] = hashlib.sha256(unknown).hexdigest()
        self.write_manifest()
        with self.assertRaisesRegex(PATCHER.PatchError, "unknown target"):
            self.apply()
        self.assertEqual((self.source / "src/a.rs").read_bytes(), self.before["src/a.rs"])

    def test_mixed_before_after_state_is_rejected(self):
        (self.source / "src/a.rs").write_bytes(self.after["src/a.rs"])
        with self.assertRaisesRegex(PATCHER.PatchError, "mixed before/after"):
            self.apply()
        self.assertEqual((self.source / "src/b.rs").read_bytes(), self.before["src/b.rs"])

    def test_unexpected_postimage_is_retained_and_reported(self):
        self.manifest["files"][0]["after_sha256"] = hashlib.sha256(b"unexpected\n").hexdigest()
        self.write_manifest()
        with self.assertRaisesRegex(PATCHER.PatchError, "unexpected source hash"):
            self.apply()
        # Failure is diagnostic and non-destructive: the applied files remain for inspection.
        self.assertEqual((self.source / "src/a.rs").read_bytes(), self.after["src/a.rs"])
        self.assertEqual((self.source / "src/b.rs").read_bytes(), self.after["src/b.rs"])

    def test_archive_mode_applies_inside_an_ancestor_git_repository(self):
        _outer, source, archive = self.make_archive_source()
        self.archive_manifest(archive)
        result = PATCHER.apply(source, self.manifest_path, archive)
        self.assertEqual(result["status"], "applied")
        self.assertEqual(result["source_kind"], "archive")
        for name, expected in self.after.items():
            self.assertEqual((source / name).read_bytes(), expected)

    def test_git_mode_rejects_ancestor_repository_even_when_head_matches(self):
        outer, source, _archive = self.make_archive_source()
        subprocess.run(["git", "-C", str(outer), "add", ".upstream"], check=True)
        subprocess.run(["git", "-C", str(outer), "-c", "user.email=patch@example.invalid",
                        "-c", "user.name=Patch Test", "commit", "-q", "-m", "outer"], check=True)
        outer_commit = subprocess.run(["git", "-C", str(outer), "rev-parse", "HEAD"],
                                      check=True, text=True, stdout=subprocess.PIPE).stdout.strip()
        self.manifest["upstream_commit"] = outer_commit
        self.write_manifest()
        with self.assertRaisesRegex(PATCHER.PatchError, "exact Git checkout root"):
            PATCHER.apply(source, self.manifest_path)
        self.assertEqual((source / "src/a.rs").read_bytes(), self.before["src/a.rs"])

    def test_archive_preimage_hash_is_verified_without_writing_source(self):
        _outer, source, archive = self.make_archive_source()
        manifest = self.archive_manifest(archive)
        manifest["files"][0]["before_sha256"] = "0" * 64
        self.manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(PATCHER.PatchError, "before_sha256 does not match"):
            PATCHER.apply(source, self.manifest_path, archive)
        self.assertEqual((source / "src/a.rs").read_bytes(), self.before["src/a.rs"])

    def test_archive_missing_and_duplicate_targets_are_rejected(self):
        _outer, source, archive = self.make_archive_source()
        self.write_archive(archive, ["src/a.rs"])
        self.archive_manifest(archive)
        with self.assertRaisesRegex(PATCHER.PatchError, "missing one or more"):
            PATCHER.apply(source, self.manifest_path, archive)
        self.write_archive(archive, ["src/a.rs", "src/a.rs", "src/b.rs"])
        self.archive_manifest(archive)
        with self.assertRaisesRegex(PATCHER.PatchError, "duplicate upstream archive target"):
            PATCHER.apply(source, self.manifest_path, archive)
        self.assertEqual((source / "src/a.rs").read_bytes(), self.before["src/a.rs"])

    def test_manifest_symlink_is_rejected_before_resolution(self):
        link = self.root / "manifest-link.json"
        link.symlink_to(self.manifest_path)
        with self.assertRaisesRegex(PATCHER.PatchError, "manifest cannot be a symlink"):
            PATCHER.apply(self.source, link)


if __name__ == "__main__":
    unittest.main()
