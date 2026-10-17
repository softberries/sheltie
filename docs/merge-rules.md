# Merge rules

When a pull request conflicts with what landed and the merge engine is on (see
[Workflows](workflows.md)), the `sheltie-merge` workflow merges it on your runner:

1. **Structural merge.** Conflicted source files go to
   [mergiraf](https://mergiraf.org), which merges by syntax tree. It is GPL-3.0 and only ever
   runs as a separate program.
2. **Derived files** are regenerated instead of merged.
3. **Declared rules** merge files that have a known shape.

The result is verified like any other change before it lands. Files no tier can resolve
return the pull request to its author, or go to [intent replay](intent-replay.md) if it is
enabled.

Rules live in `.sheltie/merge.toml` on the **default branch**. Sheltie never reads them from
the pull request being merged. They are parsed strictly: unknown keys, invalid globs or
regular expressions, files over 64 KB and more than 100 rules are rejected.

```toml
[[derived]]          # regenerate instead of merging
paths = ["Cargo.lock"]
regenerate = "cargo update -w --offline"

[[set]]              # order-insensitive lines: union of both sides
paths = ["config/feature_flags.txt"]

[[sequence]]         # numbered files: renumber the later side on collision
paths = ["migrations/*.sql"]
key = "^(\\d+)_"
```

## `[[derived]]`

Files generated from others, such as lockfiles. The engine takes one side, then runs
`regenerate` (one line, at most 500 characters, run with `sh -c` in the checkout). This also
happens when both sides changed the file without a textual conflict. A command shared by
several conflicted files runs once.

## `[[set]]`

Files whose lines form an unordered set (feature flags, allow lists, `CODEOWNERS`-style
lists). Conflicts are resolved with a three-way union of lines: lines either side added are
kept (ours first), lines either side removed are dropped, and duplicate lines are removed.

## `[[sequence]]`

Numbered files, such as database migrations, where both sides added a file with the same
number. `key` is a regular expression whose first capture group is the number; the file the
later side added is renumbered to the next free number.

When more than one rule matches a path, `[[derived]]` wins over `[[set]]`, which wins over
`[[sequence]]`.

## `[replay]`

Settings for tier 4, re-deriving a change with an agent. See [Intent replay](intent-replay.md).
