-- M3: what actually happened on the default branch, to compare with shadow predictions.

-- CI result of each default-branch commit, refreshed from GitHub's status rollup.
-- 'none' means the commit has no checks; it is not polled again.
ALTER TABLE branch_commits ADD COLUMN ci_state TEXT
    CHECK (ci_state IN ('pending', 'success', 'failure', 'error', 'none'));
ALTER TABLE branch_commits ADD COLUMN ci_checked_at TIMESTAMPTZ;
ALTER TABLE branch_commits ADD COLUMN ci_next_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE branch_commits ADD COLUMN ci_attempts INTEGER NOT NULL DEFAULT 0;

CREATE INDEX branch_commits_ci_due_idx ON branch_commits (ci_next_at)
    WHERE ci_state IS NULL OR ci_state = 'pending';

-- A pair of changes Sheltie judged independent that landed between the last green and a red
-- commit. "Suspected": without bisect (M4) the pair is not proven to be the cause.
CREATE TABLE footprint_misses (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id      BIGINT NOT NULL REFERENCES repositories (id),
    red_sha      git_oid NOT NULL,
    green_sha    git_oid,
    change_a     BIGINT NOT NULL REFERENCES changes (id),
    change_b     BIGINT NOT NULL REFERENCES changes (id),
    writes_a     TEXT[] NOT NULL,
    writes_b     TEXT[] NOT NULL,
    detected_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (change_a < change_b),
    UNIQUE (repo_id, red_sha, change_a, change_b)
);
