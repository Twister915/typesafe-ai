"""Prepare one stable release without changing dependency resolutions or Git refs."""

import argparse
from datetime import datetime, timezone
from pathlib import Path
import re
import tomllib


def version_tuple(version):
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("version must be X.Y.Z without a v prefix or prerelease suffix")
    return tuple(map(int, version.split(".")))


def replace_version(section, old, new):
    updated, count = re.subn(
        rf'(?m)^version = "{re.escape(old)}"$', f'version = "{new}"', section
    )
    if count != 1:
        raise ValueError("expected exactly one version line in the package section")
    return updated


def prepare(root, version, today):
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    changelog_path = root / "CHANGELOG.md"
    manifest = manifest_path.read_text()
    lock = lock_path.read_text()
    changelog = changelog_path.read_text()
    package = tomllib.loads(manifest)["package"]
    old = package["version"]
    if version_tuple(version) <= version_tuple(old):
        raise ValueError(f"version must be greater than {old}")

    # Edit only the root package's sections; preserve formatting and dependencies.
    manifest_parts = re.split(r"(?m)(?=^\[)", manifest)
    package_sections = [i for i, part in enumerate(manifest_parts) if part.startswith("[package]\n")]
    if len(package_sections) != 1:
        raise ValueError("expected one [package] section")
    index = package_sections[0]
    manifest_parts[index] = replace_version(manifest_parts[index], old, version)

    lock_parts = re.split(r"(?m)(?=^\[\[package\]\])", lock)
    matches = []
    for index, part in enumerate(lock_parts):
        if not part.startswith("[[package]]"):
            continue
        entry = tomllib.loads(part)["package"][0]
        if entry["name"] == package["name"] and "source" not in entry:
            if entry["version"] != old:
                raise ValueError("Cargo.lock root version does not match Cargo.toml")
            matches.append(index)
    if len(matches) != 1:
        raise ValueError("expected one root package in Cargo.lock")
    index = matches[0]
    lock_parts[index] = replace_version(lock_parts[index], old, version)

    headings = list(re.finditer(r"(?m)^## (.+)$", changelog))
    if not headings or headings[0].group(1) != "Unreleased":
        raise ValueError("CHANGELOG.md must start with an Unreleased section")
    if sum(heading.group(1) == "Unreleased" for heading in headings) != 1:
        raise ValueError("expected exactly one Unreleased section")
    if any(heading.group(1).split(" — ")[0] == version for heading in headings):
        raise ValueError("version already exists in CHANGELOG.md")
    start = headings[0]
    end = headings[1].start() if len(headings) > 1 else len(changelog)
    notes = changelog[start.end():end].strip()
    if not notes or not re.search(r"(?m)^- \S", notes):
        raise ValueError("Unreleased must contain release notes as bullet points")
    changelog = (
        changelog[:start.start()]
        + f"## Unreleased\n\n## {version} — {today}\n\n{notes}\n\n"
        + changelog[end:]
    )

    # Validate everything before writing any file.
    manifest_path.write_text("".join(manifest_parts))
    lock_path.write_text("".join(lock_parts))
    changelog_path.write_text(changelog)
    return notes + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--notes", type=Path, required=True)
    args = parser.parse_args()
    try:
        notes = prepare(Path.cwd(), args.version, datetime.now(timezone.utc).date().isoformat())
    except (ValueError, KeyError) as error:
        parser.exit(1, f"Release preparation failed: {error}\n")
    args.notes.write_text(notes)
