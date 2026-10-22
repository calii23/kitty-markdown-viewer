#!/bin/bash

set -eu

die() {
  echo "Error: $1" >&2
  exit 1
}

if [ "$#" -ne 1 ]; then
  die "Expect single argument with new version!"
fi

VERSION="$1"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  die "Expect version to be X.X.X format, got: $VERSION"
fi

if [ -n "$(git status --porcelain)" ]; then
  die "Local changes detected!"
fi

if [ "$(git rev-parse --abbrev-ref HEAD)" != "main" ]; then
  die "Not on main branch!"
fi

git fetch --tags origin

git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null && die "Tag v$VERSION already exists!"

git merge-base --is-ancestor origin/main main || die "Local 'main' is behind or diverged from 'origin/main'!"

cargo set-version "$VERSION"

git add Cargo.toml Cargo.lock
git commit -m "bump v$VERSION"
git tag -a "v$VERSION" -m "v$VERSION"
git push --atomic origin main "v$VERSION"
