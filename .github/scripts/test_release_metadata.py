"""Check that only the exact merged release version can be finalized."""

from copy import deepcopy
from pathlib import Path
import shutil
import tempfile
import tomllib
import unittest

from prepare_release import prepare
from release_metadata import merged_release, release_notes


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self):
        self.repository = "owner/repo"
        self.pr = {
            "merged": True, "state": "closed", "merge_commit_sha": "a" * 40,
            "base": {"ref": "main", "repo": {"full_name": self.repository}},
            "head": {"ref": "release/v0.2.0", "repo": {"full_name": self.repository}},
        }

    def test_uses_merge_commit_not_branch_head(self):
        self.pr["head"]["sha"] = "b" * 40
        self.assertEqual(merged_release(self.pr, self.repository), ("0.2.0", "a" * 40))

    def test_rejects_unmerged_foreign_or_malformed_pr(self):
        cases = []
        for key, value in (("merged", False), ("state", "open"), ("merge_commit_sha", None)):
            case = deepcopy(self.pr)
            case[key] = value
            cases.append(case)
        for side, key, value in (
            ("base", "ref", "other"), ("head", "ref", "feature"),
            ("head", "ref", "release/v0.2.0\nsha=bad"),
            ("head", "repo", None),
            ("head", "repo", {"full_name": "fork/repo"}),
            ("base", "repo", {"full_name": "other/repo"}),
        ):
            case = deepcopy(self.pr)
            case[side][key] = value
            cases.append(case)
        for case in cases:
            with self.subTest(pr=case), self.assertRaises(ValueError):
                merged_release(case, self.repository)

    def test_reads_exact_release_section_and_rejects_mismatches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = Path(__file__).resolve().parents[2]
            for name in ("Cargo.toml", "Cargo.lock"):
                shutil.copyfile(repo / name, root / name)
            old = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
            major, minor, patch = map(int, old.split("."))
            version = f"{major}.{minor}.{patch + 1}"
            (root / "CHANGELOG.md").write_text("# Changelog\n\n## Unreleased\n\n- Release notes.\n")
            prepare(root, version, "2026-09-16")
            self.assertEqual(release_notes(root, version), "- Release notes.")
            with self.assertRaises(ValueError):
                release_notes(root, old)
            changelog = (root / "CHANGELOG.md").read_text()
            (root / "CHANGELOG.md").write_text(changelog.replace("- Release notes.", ""))
            with self.assertRaises(ValueError):
                release_notes(root, version)
            (root / "CHANGELOG.md").write_text(changelog)
            lock = (root / "Cargo.lock").read_text().replace(
                f'name = "typesafe-ai"\nversion = "{version}"',
                f'name = "typesafe-ai"\nversion = "{old}"',
            )
            (root / "Cargo.lock").write_text(lock)
            with self.assertRaises(ValueError):
                release_notes(root, version)


if __name__ == "__main__":
    unittest.main()
