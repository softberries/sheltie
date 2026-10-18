#!/usr/bin/env bash
# Builds sheltie.dev into target/site: the landing page from site/ and the docs (mdBook) under
# /docs. Needs mdbook. Preview with: python3 -m http.server -d target/site
set -euo pipefail
cd "$(dirname "$0")/.."
out=target/site
rm -rf "$out"
mkdir -p "$out"
cp -R site/. "$out/"
mdbook build docs --dest-dir "$PWD/$out/docs"
rm -f "$out/docs/book.toml"
echo "built $out"
