"""Validate a merged release PR and read release notes from its exact commit."""

import json
from pathlib import Path
import re
import sys
import tomllib

from prepare_release import version_tuple


def merged_release(pr, repository):
    if not pr.get("merged") or pr.get("state") != "closed":
        raise ValueError("release PR must be merged")
    if pr["base"]["repo"]["full_name"] != repository or pr["base"]["ref"] != "main":
        raise ValueError("release PR must target this repository's main branch")
    if not pr["head"].get("repo") or pr["head"]["repo"]["full_name"] != repository:
        raise ValueError("release PR must originate in this repository")
    branch = pr["head"]["ref"]
    if not branch.startswith("release/v"):
        raise ValueError("expected a release/vX.Y.Z branch")
    version = branch.removeprefix("release/v")
    version_tuple(version)
    sha = pr.get("merge_commit_sha") or ""
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("missing merge commit SHA")
    return version, sha


def release_notes(root, version):
    version_tuple(version)
    package = tomllib.loads((root / "Cargo.toml").read_text())["package"]
    if package["version"] != version:
        raise ValueError("merged manifest does not match the release branch version")
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    entries = [p for p in lock["package"] if p["name"] == package["name"] and "source" not in p]
    if len(entries) != 1 or entries[0]["version"] != version:
        raise ValueError("merged lockfile does not match the release version")
    changelog = (root / "CHANGELOG.md").read_text()
    headings = list(re.finditer(r"(?m)^## (.+)$", changelog))
    matches = [i for i, h in enumerate(headings) if re.fullmatch(rf"{re.escape(version)} — \d{{4}}-\d{{2}}-\d{{2}}", h.group(1))]
    if len(matches) != 1:
        raise ValueError("expected one dated changelog section for the release")
    index = matches[0]
    end = headings[index + 1].start() if index + 1 < len(headings) else len(changelog)
    notes = changelog[headings[index].end():end].strip()
    if not re.search(r"(?m)^- \S", notes):
        raise ValueError("release changelog section must contain notes")
    return notes


if __name__ == "__main__":
    try:
        if len(sys.argv) == 4 and sys.argv[1] == "pr":
            version, sha = merged_release(json.loads(Path(sys.argv[2]).read_text()), sys.argv[3])
            print(f"version={version}\nsha={sha}")
        elif len(sys.argv) == 3 and sys.argv[1] == "notes":
            print(release_notes(Path.cwd(), sys.argv[2]))
        else:
            raise ValueError("usage: release_metadata.py pr FILE REPOSITORY | notes VERSION")
    except (ValueError, KeyError) as error:
        sys.exit(f"Release validation failed: {error}")
