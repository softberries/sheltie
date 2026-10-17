-- Time saved (dashboard): measured CI durations on the default branch, and per-repository
-- assumptions from `.sheltie/merge.toml` `[estimates]` (NULL means the defaults).
ALTER TABLE branch_commits ADD COLUMN ci_seconds INTEGER CHECK (ci_seconds >= 0);
ALTER TABLE repositories
    ADD COLUMN minutes_per_conflict INTEGER CHECK (minutes_per_conflict BETWEEN 0 AND 1440),
    ADD COLUMN minutes_per_replay INTEGER CHECK (minutes_per_replay BETWEEN 0 AND 1440);
