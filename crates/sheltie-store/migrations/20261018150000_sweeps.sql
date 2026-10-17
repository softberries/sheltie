-- M4: sweeps, culprits and auto-revert.

-- The newest default-branch commit whose CI passed: the verified prefix.
ALTER TABLE repositories ADD COLUMN verified_sha git_oid;

-- The commit a push moved the branch to. CI runs on these; commits inside a push are only
-- tested when bisect probes them.
ALTER TABLE branch_commits ADD COLUMN is_tip BOOLEAN NOT NULL DEFAULT false;

-- A suspected miss involving a commit that bisect blamed.
ALTER TABLE footprint_misses ADD COLUMN confirmed BOOLEAN NOT NULL DEFAULT false;

-- A commit blamed for turning the default branch red, and its revert.
CREATE TABLE culprits (
    repo_id      BIGINT NOT NULL REFERENCES repositories (id),
    culprit_sha  git_oid NOT NULL,
    -- The red tip that was analysed.
    red_sha      git_oid NOT NULL,
    change_id    BIGINT REFERENCES changes (id),
    state        TEXT NOT NULL CHECK (state IN ('reverting', 'reverted', 'conflict')),
    revert_sha   git_oid,
    revert_pr    BIGINT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, culprit_sha)
);
