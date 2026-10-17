#!/usr/bin/env python3
"""Fails if a relative Markdown link in README.md, the community files or docs/ points at a
file that does not exist. External links are not checked."""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = [*ROOT.glob("*.md"), *(ROOT / "docs").glob("*.md")]
LINK = re.compile(r"\]\(([^)\s]+)\)")

broken = []
for md in FILES:
    text = re.sub(r"```.*?```", "", md.read_text(), flags=re.S)  # skip code blocks
    for target in LINK.findall(text):
        if re.match(r"^[a-z]+:", target) or target.startswith("#"):
            continue
        path = (md.parent / target.split("#")[0]).resolve()
        if not path.exists():
            broken.append(f"{md.relative_to(ROOT)}: {target}")
print("\n".join(broken) or f"{len(FILES)} files, no broken relative links")
sys.exit(1 if broken else 0)
