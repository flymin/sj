#!/usr/bin/env bash
set -euo pipefail

SJ_PROJECT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$SJ_PROJECT"
python3 scripts/release.py metadata
SJ_TAG=${GITHUB_REF_NAME:?A version tag is required}
test "${GITHUB_REF:?}" = "refs/tags/$SJ_TAG"

if SJ_DRAFT=$(gh release view "$SJ_TAG" --repo "$GITHUB_REPOSITORY" --json isDraft --jq '.isDraft' 2>/dev/null); then
    if [ "$SJ_DRAFT" = false ]; then
        printf 'Release %s is already published; keeping its existing assets.\n' "$SJ_TAG"
        exit 0
    fi
else
    SJ_FLAGS=()
    if [[ "$SJ_TAG" == *-* ]]; then SJ_FLAGS+=(--prerelease); fi
    SJ_NOTES="docs/releases/${SJ_TAG#v}.md"
    if [[ -f "$SJ_NOTES" ]]; then SJ_FLAGS+=(--notes-file "$SJ_NOTES"); else SJ_FLAGS+=(--generate-notes); fi
    gh release create "$SJ_TAG" --repo "$GITHUB_REPOSITORY" --verify-tag --draft --title "sj $SJ_TAG" "${SJ_FLAGS[@]}"
fi
gh release upload "$SJ_TAG" dist/*.tar.gz dist/SHA256SUMS --repo "$GITHUB_REPOSITORY" --clobber
SJ_LATEST=true
if [[ "$SJ_TAG" == *-* ]]; then SJ_LATEST=false; fi
gh release edit "$SJ_TAG" --repo "$GITHUB_REPOSITORY" --draft=false --latest="$SJ_LATEST"
