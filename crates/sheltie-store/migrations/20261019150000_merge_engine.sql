-- M5: the merge engine runs on the repository's runner, triggered by repository_dispatch.

-- Whether the repository has the merge-engine workflow installed; without it, changes that
-- conflict are returned to their authors as before.
ALTER TABLE repositories ADD COLUMN merge_engine BOOLEAN NOT NULL DEFAULT false;

-- A change sent to the merge engine, and what it reported.
CREATE TABLE merge_runs (
    change_id     BIGINT PRIMARY KEY REFERENCES changes (id) ON DELETE CASCADE,
    onto_sha      git_oid NOT NULL,
    requested_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Set when the engine reports: the merge commit, or the files it could not resolve.
    result_sha    git_oid,
    unresolved    TEXT[],
    reported_at   TIMESTAMPTZ,
    CHECK (result_sha IS NULL OR unresolved IS NULL)
);

-- Which change a resolution belongs to (resolution_changes links the combined changes).
ALTER TABLE resolutions ADD COLUMN merge_sha git_oid;
