-- M4: Sheltie lands changes in repositories switched to merge mode.

-- 'shadow' only predicts; 'merge' lands changes. Set by an administrator, not by a file in
-- the repository, so a pull request cannot switch it.
ALTER TABLE repositories ADD COLUMN mode TEXT NOT NULL DEFAULT 'shadow'
    CHECK (mode IN ('shadow', 'merge'));

-- Why a change was returned to its author.
ALTER TABLE changes ADD COLUMN returned_reason TEXT;

-- A change merged onto the head on a verify branch, waiting for the repository's CI.
CREATE TABLE verify_runs (
    change_id    BIGINT PRIMARY KEY REFERENCES changes (id) ON DELETE CASCADE,
    branch       TEXT NOT NULL,
    verify_sha   git_oid NOT NULL,
    -- The default-branch head the change was merged onto.
    onto_sha     git_oid NOT NULL,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
