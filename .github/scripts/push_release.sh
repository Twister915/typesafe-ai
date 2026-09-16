#!/usr/bin/env bash
set -euo pipefail

: "${BASE_SHA:?expected the commit selected at dispatch}"
: "${RELEASE_VERSION:?expected the validated release version}"
test -z "$(git status --porcelain)"
# Only publish the single release commit created on top of the selected main.
test "$(git rev-parse HEAD^)" = "$BASE_SHA"
git tag -a "v$RELEASE_VERSION" -m "Release v$RELEASE_VERSION"
# The lease requires main to still match the commit selected at dispatch.
# The empty tag lease requires a new tag. Atomic push updates both or neither.
git push --atomic \
  --force-with-lease="refs/heads/main:$BASE_SHA" \
  --force-with-lease="refs/tags/v$RELEASE_VERSION:" \
  origin HEAD:refs/heads/main "refs/tags/v$RELEASE_VERSION:refs/tags/v$RELEASE_VERSION"
