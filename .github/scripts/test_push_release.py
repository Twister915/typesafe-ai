"""Verify tag publication safeguards against a disposable local bare Git remote."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class PushReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.remote = root / "remote.git"
        self.repo = root / "checkout"
        self.repo.mkdir()
        self.git("init", "--bare", str(self.remote))
        self.git("init", "--initial-branch=main")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("commit", "--allow-empty", "-m", "Base")
        self.base = self.git("rev-parse", "HEAD")
        self.git("remote", "add", "origin", str(self.remote))
        self.git("push", "origin", "main")
        self.git("commit", "--allow-empty", "-m", "Release")
        self.release = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.repo, check=True, text=True, capture_output=True
        ).stdout.strip()

    def refs(self):
        return self.git("--git-dir", str(self.remote), "show-ref")

    def push(self):
        script = Path(__file__).resolve().with_name("push_release.sh")
        return subprocess.run(
            ["bash", str(script)], cwd=self.repo, text=True, capture_output=True,
            env={**os.environ, "RELEASE_VERSION": "0.2.0"},
        )

    def test_pushes_only_the_release_tag(self):
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"{self.base} refs/heads/main", self.refs())
        target = self.git("--git-dir", str(self.remote), "rev-parse", "refs/tags/v0.2.0^{commit}")
        self.assertEqual(target, self.release)

    def test_main_can_advance_without_changing_release_target(self):
        self.git("switch", "--detach", self.base)
        self.git("commit", "--allow-empty", "-m", "Concurrent merge")
        self.git("push", "origin", "HEAD:main")
        self.git("switch", "--detach", self.release)
        main = self.git("--git-dir", str(self.remote), "rev-parse", "main")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"{main} refs/heads/main", self.refs())
        target = self.git("--git-dir", str(self.remote), "rev-parse", "v0.2.0^{commit}")
        self.assertEqual(target, self.release)

    def test_tag_race_leaves_both_refs_unchanged(self):
        self.git("--git-dir", str(self.remote), "tag", "v0.2.0", self.base)
        before = self.refs()
        self.assertNotEqual(self.push().returncode, 0)
        self.assertEqual(self.refs(), before)

    def test_retry_preserves_the_existing_tag(self):
        self.assertEqual(self.push().returncode, 0)
        before = self.refs()
        self.assertEqual(self.push().returncode, 0)
        self.assertEqual(self.refs(), before)

    def test_local_tag_on_another_commit_is_rejected(self):
        self.git("tag", "v0.2.0", self.base)
        before = self.refs()
        self.assertNotEqual(self.push().returncode, 0)
        self.assertEqual(self.refs(), before)

    def test_dirty_checkout_is_not_published(self):
        (self.repo / "unexpected.txt").write_text("unreviewed change")
        before = self.refs()
        self.assertNotEqual(self.push().returncode, 0)
        self.assertEqual(self.refs(), before)


if __name__ == "__main__":
    unittest.main()
