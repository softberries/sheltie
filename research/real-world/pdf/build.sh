#!/usr/bin/env bash
# Builds site/assets/sheltie-on-real-repositories.pdf from docs/real-world.md, with a cover
# (template.html) and the sheltie.dev style (print.css).
# Needs pandoc and Google Chrome (or Chromium; set CHROME).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); root=$(cd "$here/../../.." && pwd)
chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
tmp=$(mktemp -d)
# Links between docs pages point at the published docs.
sed -E 's/\]\(([a-z-]+)\.md(#[^)]*)?\)/](https:\/\/sheltie.dev\/docs\/\1.html\2)/g' "$root/docs/real-world.md" > "$tmp/page.md"
# Fonts and the logo come from the site.
pandoc "$tmp/page.md" -f gfm -t html5 -s --template "$here/template.html" \
  --metadata title="Sheltie on real repositories" --metadata date="October 2026" \
  --css "$here/print.css" --resource-path "$root/site:$here" --embed-resources -o "$tmp/page.html"
out="$root/site/assets/sheltie-on-real-repositories.pdf"
rm -f "$out"
# A separate profile, so a running Chrome does not take the job over.
"$chrome" --headless=new --disable-gpu --no-first-run --user-data-dir="$tmp/profile" \
  --no-pdf-header-footer --print-to-pdf="$out" "file://$tmp/page.html" >"$tmp/chrome.log" 2>&1 &
pid=$!
for _ in $(seq 1 60); do [ -s "$out" ] && break; sleep 1; done
sleep 1; kill "$pid" 2>/dev/null || true
[ -s "$out" ] || { cat "$tmp/chrome.log"; exit 1; }
echo "wrote site/assets/sheltie-on-real-repositories.pdf"
