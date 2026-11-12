#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'Release aborted: %s\n' "$1" >&2
  exit 1
}

if [[ "$#" -gt 1 || ( "$#" -eq 1 && ! "$1" =~ ^(patch|minor|major)$ ) ]]; then
  echo "Usage: npm run release [-- patch|minor|major]" >&2
  exit 2
fi

BUMP="${1:-patch}"
ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this command inside the Git repository."
cd "$ROOT"

[[ "$(git branch --show-current)" == "main" ]] || fail "switch to main after merging the release PR."
[[ -z "$(git status --porcelain --untracked-files=all)" ]] || fail "commit or remove all local changes before releasing."

REMOTE_URL="$(git remote get-url origin 2>/dev/null)" || fail "configure the GitHub repository as the 'origin' remote."
[[ "$REMOTE_URL" == *github.com* ]] || fail "the 'origin' remote must point to GitHub."

git fetch --no-tags origin "+refs/heads/main:refs/remotes/origin/main"
[[ "$(git rev-parse HEAD)" == "$(git rev-parse refs/remotes/origin/main)" ]] ||
  fail "local main must exactly match origin/main. Pull the merged PR first."

git fetch --tags origin

CURRENT_VERSION="$(node -p 'require("./package.json").version')"
[[ "$CURRENT_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] ||
  fail "package.json must contain a numeric MAJOR.MINOR.PATCH version."
LATEST_VERSION="$(git tag --list 'v[0-9]*' | node -e '
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => { input += chunk; });
process.stdin.on("end", () => {
  const versions = input
    .split(/\r?\n/)
    .map((tag) => tag.match(/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/)?.slice(1))
    .filter(Boolean);
  const compare = (left, right) => {
    for (let index = 0; index < 3; index += 1) {
      const a = BigInt(left[index]);
      const b = BigInt(right[index]);
      if (a !== b) return a < b ? -1 : 1;
    }
    return 0;
  };
  versions.sort(compare);
  console.log(versions.length ? versions[versions.length - 1].join(".") : "");
});
')"

if [[ -z "$LATEST_VERSION" && "$#" -eq 0 ]]; then
  VERSION="$CURRENT_VERSION"
  BUMP_LABEL="initial"
else
  BASE_VERSION="${LATEST_VERSION:-$CURRENT_VERSION}"
  VERSION="$(node - "$BASE_VERSION" "$BUMP" <<'NODE'
const [major, minor, patch] = process.argv[2].split(".").map(BigInt);
const bump = process.argv[3];
if (bump === "major") console.log(`${major + 1n}.0.0`);
else if (bump === "minor") console.log(`${major}.${minor + 1n}.0`);
else console.log(`${major}.${minor}.${patch + 1n}`);
NODE
)"
  BUMP_LABEL="$BUMP"
fi

TAG="v${VERSION}"
LOCAL_TAG_COMMIT="$(git rev-parse --verify --quiet "${TAG}^{commit}" || true)"
if [[ -n "$LOCAL_TAG_COMMIT" && "$LOCAL_TAG_COMMIT" != "$(git rev-parse HEAD)" ]]; then
  fail "local tag $TAG already points to a different commit."
fi

REMOTE_TAG="$(git ls-remote --tags origin | awk -v ref="refs/tags/${TAG}" '$2 == ref || $2 == ref "^{}"')"
[[ -z "$REMOTE_TAG" ]] || fail "tag $TAG already exists on GitHub."

if [[ -z "$LOCAL_TAG_COMMIT" ]]; then
  git tag -a "$TAG" -m "$TAG"
fi

printf 'Selected version %s (%s).\n' "$VERSION" "$BUMP_LABEL"
printf 'Pushing %s to GitHub. This starts the release build...\n' "$TAG"
git push origin "refs/tags/${TAG}:refs/tags/${TAG}"
printf 'Tag %s pushed to GitHub.\n' "$TAG"
