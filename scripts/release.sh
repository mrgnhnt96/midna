#!/bin/bash
# Cut a release: set the version, commit, tag and push.
#
#   tb release cut v0.2.0          a public release, from main
#   tb release cut v0.3.0-beta.1   a beta, from any branch
#
# Releases go through the task board: tb release cut runs this script with
# TB_RELEASE set and records the release on the board when it succeeds. Run on
# its own, the script refuses unless you pass --force. A leading v is optional.
#
# Pushing the v0.2.0 tag runs .github/workflows/release.yml, which builds,
# signs and notarizes Midna.app and publishes the GitHub release with the DMG
# and the update archive attached. The website's Download button and the
# app's updater both read that release.
#
# A beta is published as a prerelease: the website and the stable channel
# skip it, and only copies with Settings › Updates › Channel set to beta get it.
set -euo pipefail
cd "$(dirname "$0")/.."

force=
args=()
for arg in "$@"; do
  if [ "$arg" = --force ]; then force=1; else args+=("$arg"); fi
done
version="${args[0]:-}"
version="${version#v}"
if [ -z "${TB_RELEASE:-}" ] && [ -z "$force" ]; then
  echo "Run tb release cut v${version:-<version>} instead; it runs this script and records the release. (--force to run it without the board.)" >&2
  exit 1
fi
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]]; then
  echo "Usage: $0 [v]<major.minor.patch[-beta.N]> [--force]" >&2
  exit 1
fi
tag="v$version"

branch=$(git rev-parse --abbrev-ref HEAD)
if [[ "$version" != *-beta.* && "$branch" != main ]]; then
  echo "Release from main (on $branch). Betas can go out from any branch." >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "Commit or stash your changes first." >&2
  exit 1
fi
git fetch --tags origin
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "$tag already exists." >&2
  exit 1
fi
# A public release only has to be newer than the last public one: a hotfix
# can go out while a newer beta is out, and beta copies stay on the beta.
# A beta has to be newer than everything, or beta copies would never get it.
tags=$(git tag --list 'v[0-9]*' | sed 's/^v//' | grep -E '^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$' || true)
[[ "$version" == *-beta.* ]] || tags=$(echo "$tags" | grep -v -- '-beta\.' || true)
latest=
[ -z "$tags" ] || latest="v$(./scripts/newest-version.py $tags)"
if [ -n "$latest" ] && [ "$(./scripts/newest-version.py "${latest#v}" "$version")" != "$version" ]; then
  echo "$tag isn't newer than $latest." >&2
  exit 1
fi
# Never skip a version. A tag whose release was never published (its workflow
# failed or was cancelled) gets its number re-cut, not a new one after it.
if [ -n "$latest" ] && ! gh release view "$latest" >/dev/null 2>&1; then
  echo "$latest has no GitHub release: its workflow failed, was cancelled or is still running." >&2
  echo "Don't skip it. Once no run is going, delete its tag and cut it again:" >&2
  echo "  git push origin :refs/tags/$latest && git tag -d $latest && tb release cut $latest" >&2
  exit 1
fi

./scripts/set-version.sh "$version"
git add Cargo.toml Cargo.lock
# Re-cutting a version main already has (see above) has nothing to commit.
git diff --cached --quiet || git commit -m "chore: release $tag"
git tag -a "$tag" -m "Midna $version"
git push origin "$branch" "$tag"

echo
echo "Pushed $tag. The release workflow builds the app and publishes the release:"
echo "  https://github.com/mrgnhnt96/midna/actions/workflows/release.yml"
