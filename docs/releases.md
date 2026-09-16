# Releases

`main` contains the latest release plus unreleased changes. Tags such as `v0.1.0`
identify frozen releases. Merge normal PRs into `main` whenever they are ready;
there is no release branch or post-release development-version bump.

Add user-facing changes to `CHANGELOG.md` under `## Unreleased` as bullet points.
Leave `Cargo.toml` and `Cargo.lock` at the last release version until cutting the
next release. For this pre-1.0 crate, use a minor bump for breaking API changes
(for example, `0.1.0` to `0.2.0`) and a patch bump for compatible changes.

## Cut a release on GitHub

After the release workflow has been merged into `main`:

1. Open **Actions → Release → Run workflow** in GitHub.
2. Select **main** and enter the exact version, such as `0.2.0`, without `v`.
3. Click **Run workflow** and wait for it to succeed.

Only stable `X.Y.Z` versions greater than the current manifest version are
supported. The `Unreleased` section must contain notes, and the tag must not exist.
The workflow checks out the exact `main` commit selected when the run was started.
It updates only the root package version in both Cargo files, dates the changelog
section using UTC, leaves an empty `Unreleased` section, and creates a release
commit. Dependency versions remain unchanged.

Before pushing, it runs the same formatting, Clippy, tests, documentation, package,
and feature checks as CI. CI and releases share `.github/scripts/check.sh`.
The release runs the five feature selections sequentially; normal CI uses a matrix.
The Python preparation tests run in both workflows and need Python 3.11 or newer.

The commit and annotated tag are pushed atomically: both are created or neither
is. An explicit lease requires `main` to still point to the selected commit and
the tag to remain absent. If another PR merges during validation, the push fails
without overwriting it. Start a **new** workflow run to select the new `main`;
rerunning the old run still selects its original commit. Release runs are serialized.

Finally, the workflow creates a GitHub Release using the changelog notes and puts
local publishing commands in the run summary. This does **not** publish to crates.io.
The GitHub Release can exist before the crate becomes available in the registry.

GitHub supplies the workflow's `GITHUB_TOKEN`; no personal token or crates.io
secret needs to be configured. The workflow requests `contents: write`, and
repository or organization rules must allow it to push to `main` and create tags.
It does not bypass branch protection. If protection is enabled later, revisit
this direct-commit release flow. Token-authenticated pushes do not trigger ordinary
push CI, so all checks run inside the release workflow before the push.

## Publish from your machine

Publish from the tag, even if `main` has advanced. For example, from your repository:

```sh
git fetch origin --tags
git worktree add --detach ../typesafe-ai-release-0.2.0 v0.2.0
cd ../typesafe-ai-release-0.2.0
cargo publish --locked --dry-run
cargo publish --locked
```

Use your local Cargo credentials. Run the final command only after reviewing the
dry run. Cargo's package verification builds the packaged crate; it does not rerun
the full CI suite. The suite already ran on the tagged source before release.

## Recover from failures

- **Before the atomic push:** no remote release commit or tag was created. Fix the
  issue and start a new run from `main` with the intended version.
- **After the push, before GitHub Release creation:** the release commit and tag
  already exist. Do not move or delete the tag. Inspect the tag and create the
  missing GitHub Release in the UI using that existing tag and its changelog notes.
  A new workflow run with the same version deliberately fails instead of retagging.
- **Local publishing fails:** fix local authentication or connectivity and retry
  from the same tag. If the upload result was ambiguous, check crates.io first.
  If a source change is needed, merge the fix into `main` and cut a new version;
  never change an existing release tag.
