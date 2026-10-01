#!/usr/bin/env bash
# Publish an immutable tag, or verify a byte-identical rerun without mutations.
set -euo pipefail
release_tag=${1:?release tag required}
artifact_dir=${2:?artifact directory required}
[[ "$release_tag" == v* ]] || { echo 'Expected a version tag' >&2; exit 1; }
if gh release view "$release_tag" >/dev/null 2>&1; then
  existing_dir=$(mktemp -d)
  trap 'rm -rf "$existing_dir"' EXIT
  gh release download "$release_tag" --dir "$existing_dir"
  diff --recursive --brief "$artifact_dir" "$existing_dir" || {
    echo "Refusing to replace immutable assets for $release_tag" >&2
    exit 1
  }
  echo "Existing $release_tag assets are byte-identical; no changes made"
elif [[ "$release_tag" == *-* ]]; then
  gh release create "$release_tag" "$artifact_dir"/* --verify-tag --generate-notes --prerelease --latest=false
else
  gh release create "$release_tag" "$artifact_dir"/* --verify-tag --generate-notes --latest
fi
