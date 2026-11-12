#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'Release aborted: %s\n' "$1" >&2
  exit 1
}

if [[ "$#" -ne 1 || ! "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Usage: npm run release -- <version> (example: npm run release -- 0.1.1)" >&2
  exit 2
fi

VERSION="$1"
TAG="v${VERSION}"
ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this command inside the Git repository."
cd "$ROOT"

[[ "$(git branch --show-current)" == "main" ]] || fail "switch to the main branch first."
[[ -z "$(git status --porcelain --untracked-files=all)" ]] || fail "commit or remove all local changes before releasing."

for remote in origin github; do
  git remote get-url "$remote" >/dev/null 2>&1 || fail "required remote '$remote' is not configured."
  git fetch --no-tags "$remote" "+refs/heads/main:refs/remotes/${remote}/main"
  remote_ref="refs/remotes/${remote}/main"
  remote_commit="$(git rev-parse "$remote_ref")"
  git merge-base --is-ancestor "$remote_commit" HEAD ||
    fail "local main does not contain $remote/main; reconcile the branches before releasing."
done

node - "$VERSION" "$(node -p 'require("./package.json").version')" <<'NODE'
const [target, current] = process.argv.slice(2).map((version) => version.split(".").map(BigInt));
const compare = (left, right) => {
  for (let index = 0; index < 3; index += 1) {
    if (left[index] !== right[index]) return left[index] < right[index] ? -1 : 1;
  }
  return 0;
};

if (compare(target, current) < 0) {
  console.error(`Release version ${process.argv[2]} is lower than package version ${process.argv[3]}.`);
  process.exit(1);
}
NODE

current_version="$(node -p 'require("./package.json").version')"
local_tag_commit="$(git rev-parse --verify --quiet "refs/tags/${TAG}^{commit}" || true)"
if [[ "$current_version" != "$VERSION" && -n "$local_tag_commit" ]]; then
  fail "$TAG already exists locally, but the project version still needs to change."
fi
if [[ -n "$local_tag_commit" && "$local_tag_commit" != "$(git rev-parse HEAD)" ]]; then
  fail "local tag $TAG already points to a different commit."
fi

remote_tag_commit() {
  local remote="$1"
  git ls-remote --tags "$remote" "refs/tags/${TAG}" "refs/tags/${TAG}^{}" |
    awk -v ref="refs/tags/${TAG}" '
      $2 == ref "^{}" { peeled = $1 }
      $2 == ref { direct = $1 }
      END { if (peeled != "") print peeled; else if (direct != "") print direct }
    '
}

for remote in origin github; do
  remote_tag="$(remote_tag_commit "$remote")"
  if [[ "$current_version" != "$VERSION" && -n "$remote_tag" ]]; then
    fail "$remote already has $TAG, but the project version still needs to change."
  fi
  if [[ -n "$remote_tag" && ( -z "$local_tag_commit" || "$remote_tag" != "$local_tag_commit" ) ]]; then
    fail "$remote already has $TAG pointing to a different commit."
  fi
done

if [[ "$current_version" != "$VERSION" ]]; then
  "$ROOT/version.sh" "$VERSION"
  npm run build
  git add package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
  git commit -m "chore(release): ${TAG}"
fi

RELEASE_COMMIT="$(git rev-parse HEAD)"
for remote in origin github; do
  git fetch --no-tags "$remote" "+refs/heads/main:refs/remotes/${remote}/main"
  remote_commit="$(git rev-parse "refs/remotes/${remote}/main")"
  git merge-base --is-ancestor "$remote_commit" "$RELEASE_COMMIT" ||
    fail "$remote/main advanced during release; reconcile the branches before continuing."
done

printf 'Pushing main to Gitea...\n'
git push origin HEAD:refs/heads/main
printf 'Pushing main to GitHub...\n'
git push github HEAD:refs/heads/main

if [[ -z "$local_tag_commit" ]]; then
  git tag -a "$TAG" -m "${TAG}"
fi

[[ "$(git rev-parse "${TAG}^{commit}")" == "$RELEASE_COMMIT" ]] ||
  fail "tag $TAG does not point to the release commit."

printf 'Pushing %s tag to Gitea...\n' "$TAG"
git push origin "refs/tags/${TAG}:refs/tags/${TAG}"
printf 'Pushing %s tag to GitHub (this starts the build)...\n' "$TAG"
git push github "refs/tags/${TAG}:refs/tags/${TAG}"

printf 'Release %s pushed to Gitea and GitHub.\n' "$TAG"
