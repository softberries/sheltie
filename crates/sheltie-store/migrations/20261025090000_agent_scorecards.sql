-- Who opened each pull request, its branch and labels, for the agent scorecards (NULL for
-- pull requests seen before this was recorded).
ALTER TABLE changes
    ADD COLUMN author_login TEXT CHECK (char_length(author_login) BETWEEN 1 AND 64),
    ADD COLUMN author_bot   BOOLEAN,
    ADD COLUMN head_ref     TEXT CHECK (char_length(head_ref) BETWEEN 1 AND 255),
    ADD COLUMN labels       TEXT[] CHECK (cardinality(labels) <= 50);

-- The model a replay's agent used, as reported by the runner.
ALTER TABLE replay_runs
    ADD COLUMN model TEXT CHECK (char_length(model) BETWEEN 1 AND 100);

-- `[[agents]]` from `.sheltie/merge.toml`, validated (NULL: none).
ALTER TABLE repositories
    ADD COLUMN agent_rules JSONB;
