"""Puts the generated tables (results/tables.md) into docs/real-world.md, between the
`<!-- tables:<name> -->` and `<!-- /tables:<name> -->` markers.

usage: python3 fill_docs.py
"""
import os, re

HERE = os.path.dirname(os.path.abspath(__file__))
DOC = os.path.join(HERE, "..", "..", "docs", "real-world.md")
tables = {}
for block in re.split(r"^## ", open(os.path.join(HERE, "results", "tables.md")).read(), flags=re.M)[1:]:
    name, _, body = block.partition("\n")
    tables[name.strip()] = body.strip()
doc = open(DOC).read()
for name, body in tables.items():
    doc, n = re.subn(rf"(<!-- tables:{name} -->\n).*?(<!-- /tables:{name} -->)",
                     lambda m: f"{m.group(1)}{body}\n{m.group(2)}", doc, flags=re.S)
    print(name, "filled" if n else "no marker")
open(DOC, "w").write(doc)
