#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_VERSION:?expected the validated release version}"
[[ "$RELEASE_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
test -z "$(git status --porcelain)"
# Retry only when the existing tag points to this exact merged commit.
if git show-ref --verify --quiet "refs/tags/v$RELEASE_VERSION"; then
  test "$(git rev-parse "refs/tags/v$RELEASE_VERSION^{commit}")" = "$(git rev-parse HEAD)"
else
  git tag -a "v$RELEASE_VERSION" -m "Release v$RELEASE_VERSION"
fi
# Never push main, move an existing tag, or depend on main staying at the release.
git push origin "refs/tags/v$RELEASE_VERSION:refs/tags/v$RELEASE_VERSION"
