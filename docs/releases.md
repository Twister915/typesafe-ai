# Releases

`main` contains the latest release plus unreleased changes. Tags such as `v0.1.0`
identify frozen releases. Merge normal PRs into `main` whenever they are ready.

Add user-facing changes to `CHANGELOG.md` under `## Unreleased` as bullet points.
Leave `Cargo.toml` and `Cargo.lock` at the last release version until preparing the
next release. For this pre-1.0 crate, use a minor bump for breaking API changes
(for example, `0.1.0` to `0.2.0`) and a patch bump for compatible changes.

## Repository setup

Keep the `main` ruleset enabled, including required PRs and status checks. No
bypass actor or token is needed. Enable these two repository settings:

- **Settings → General → Pull Requests → Allow auto-merge**.
- **Settings → Actions → General → Workflow permissions → Allow GitHub Actions
  to create and approve pull requests**. GitHub combines these capabilities in
  one checkbox; these workflows create PRs but never submit approving reviews.

The default workflow permission can remain read-only. Individual jobs request
only the permissions they need: preparing a release needs contents/PR write and
Actions write to dispatch CI; finalization needs contents write and PR read.
GitHub supplies `GITHUB_TOKEN`; no personal token or crates.io secret is required.

## Cut a release on GitHub

1. Open **Actions → Release → Run workflow**.
2. Select **main** and enter a stable version, such as `0.2.0`, without `v`.
3. Click **Run workflow**. The run summary links to the release PR.

The workflow prepares `release/vX.Y.Z` with the root package version updated in
both Cargo files, the release notes dated in UTC, and an empty `Unreleased`
section. Dependency versions stay unchanged. It pushes only that new branch and
opens a PR targeting `main`; it never directly updates the protected branch.
Existing release branches and tags are not overwritten.

The workflow explicitly dispatches CI on the release branch and waits for the
entire run to pass. This runs the existing `cargo fmt`, `check`, and feature
matrix jobs on the PR head, including required status checks. Token-authenticated
pushes do not trigger ordinary push CI; explicit dispatch also avoids depending
on the approval-required CI run that GitHub may create for a bot-authored PR.

After CI succeeds, the workflow enables squash auto-merge for that exact PR head.
GitHub still enforces required checks, reviews, and an up-to-date branch. If any
requirement remains unmet, the PR stays open. If `main` advances, update the PR
branch and let CI pass again; no workflow bypasses the rules or approves reviews.

After merge, **Finalize release** checks out the PR's exact merge commit,
validates its version/changelog and runs the full crate and feature checks again.
It then pushes only an annotated tag and creates a GitHub Release. The tag refers
to that merged commit even if `main` advances. Crates.io publishing remains manual.
The GitHub Release can exist before the crate is available in the registry.

The dispatching workflow calls finalization directly because merges performed
with `GITHUB_TOKEN` may not trigger another workflow. A merged-PR event also
handles human merges. Finalization is serialized per PR and can safely repeat:
an existing tag must identify the exact same commit, and an existing GitHub
Release is preserved.

## Publish from your machine

Publish from the tag, even if `main` has advanced. From your repository:

```sh
git fetch origin --tags
git worktree add --detach ../typesafe-ai-release-0.2.0 v0.2.0
cd ../typesafe-ai-release-0.2.0
cargo publish --locked --dry-run
cargo publish --locked
```

Use your local Cargo credentials and review the dry run before publishing.
The finalization run summary includes these commands with the actual version.

## Recover from failures

- **Branch created, PR creation failed:** enable the Actions PR setting above,
  then open a PR from the compare link in the run summary. Do not rerun release
  preparation over the existing branch.
- **CI fails or auto-merge is blocked:** fix or update the release PR, rerun CI,
  and enable auto-merge once all checks pass. Normal branch rules still apply.
- **Finalization times out waiting for merge:** it waits up to ten minutes for
  remaining requirements. Once merged, run **Actions → Finalize release → Run
  workflow** on `main`, entering the release PR number. Use the same recovery
  button after cancellation or a tag/Release API failure; it reads the actual
  merged SHA and refuses unmerged, foreign, or mismatched release PRs.
- **A tag already points elsewhere:** investigate and cut a new version rather
  than moving the tag. No workflow overwrites an existing tag.
- **Local publishing fails:** resolve local credentials or connectivity and
  retry from the same tag. If the upload result was ambiguous, check crates.io
  first. Source fixes need a new PR and release version.

The preparation and finalization tests run in CI and require Python 3.11 or newer.
