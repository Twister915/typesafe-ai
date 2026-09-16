"""Exercise release preparation using disposable copies of the real package files."""

from pathlib import Path
import shutil
import tempfile
import tomllib
import unittest

from prepare_release import prepare


class PrepareReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        repo = Path(__file__).resolve().parents[2]
        self.files = ("Cargo.toml", "Cargo.lock", "CHANGELOG.md")
        for name in self.files:
            shutil.copyfile(repo / name, self.root / name)
        # Keep tests valid after future releases reset Unreleased to an empty section.
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## Unreleased\n\n- A useful change.\n\n"
            "## 0.0.1 — 2026-01-01\n\n- Earlier release.\n"
        )
        self.old = tomllib.loads((self.root / "Cargo.toml").read_text())["package"]["version"]
        major, minor, patch = map(int, self.old.split("."))
        self.new = f"{major}.{minor}.{patch + 1}"

    def contents(self):
        return {name: (self.root / name).read_text() for name in self.files}

    def assert_rejected_without_changes(self, version):
        before = self.contents()
        with self.assertRaises(ValueError):
            prepare(self.root, version, "2026-09-16")
        self.assertEqual(before, self.contents())

    def test_updates_release_without_changing_dependencies(self):
        before = self.contents()
        notes = prepare(self.root, self.new, "2026-09-16")
        self.assertEqual(notes, "- A useful change.\n")
        after = self.contents()
        manifest = tomllib.loads(after["Cargo.toml"])
        self.assertEqual(manifest["package"]["version"], self.new)
        manifest["package"]["version"] = self.old
        self.assertEqual(manifest, tomllib.loads(before["Cargo.toml"]))
        lock = tomllib.loads(after["Cargo.lock"])
        root = next(p for p in lock["package"] if p["name"] == "typesafe-ai" and "source" not in p)
        self.assertEqual(root["version"], self.new)
        root["version"] = self.old
        self.assertEqual(lock, tomllib.loads(before["Cargo.lock"]))
        self.assertIn(f"## Unreleased\n\n## {self.new} — 2026-09-16\n", after["CHANGELOG.md"])
        self.assertTrue(after["CHANGELOG.md"].endswith("## 0.0.1 — 2026-01-01\n\n- Earlier release.\n"))

    def test_rejects_invalid_or_non_increasing_versions(self):
        for version in (self.old, "0.0.0", "v1.0.0", "01.0.0", "1.0", "1.0.0-rc.1", "1.0.0+build", "1.0.0\n", "$(id)"):
            with self.subTest(version=version):
                self.assert_rejected_without_changes(version)

    def test_requires_release_notes(self):
        for changelog in (
            "# Changelog\n\n## Unreleased\n\n## 0.0.1\n\n- Old notes.\n",
            "# Changelog\n\n## 0.0.1\n\n- Old notes.\n",
            "# Changelog\n\n## Unreleased\n\n- Change.\n\n## Unreleased\n\n- Duplicate.\n",
            f"# Changelog\n\n## Unreleased\n\n- Change.\n\n## {self.new} — 2026-01-01\n\n- Duplicate.\n",
        ):
            with self.subTest(changelog=changelog):
                (self.root / "CHANGELOG.md").write_text(changelog)
                self.assert_rejected_without_changes(self.new)

    def test_rejects_mismatched_lockfile(self):
        path = self.root / "Cargo.lock"
        lock = path.read_text().replace(
            f'name = "typesafe-ai"\nversion = "{self.old}"',
            'name = "typesafe-ai"\nversion = "99.0.0"',
        )
        path.write_text(lock)
        self.assert_rejected_without_changes(self.new)


if __name__ == "__main__":
    unittest.main()
