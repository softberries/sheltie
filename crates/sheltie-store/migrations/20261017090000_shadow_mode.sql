-- M3: readiness, target-branch history and shadow-mode predictions.

ALTER TABLE repositories ADD COLUMN default_branch TEXT
    CHECK (default_branch IS NULL OR length(default_branch) BETWEEN 1 AND 255);

-- Which branch the PR targets, and how it ended.
ALTER TABLE changes ADD COLUMN base_ref TEXT;
ALTER TABLE changes ADD COLUMN ready_at TIMESTAMPTZ;
ALTER TABLE changes ADD COLUMN merge_commit_sha git_oid;
ALTER TABLE changes ADD COLUMN merged_at TIMESTAMPTZ;

-- Commits on each repository's default branch, in push order. `seq` orders them so the
-- scheduler can tell what landed after a change's base.
CREATE TABLE branch_commits (
    repo_id    BIGINT NOT NULL REFERENCES repositories (id),
    sha        git_oid NOT NULL,
    seq        BIGINT NOT NULL,
    pushed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, sha),
    UNIQUE (repo_id, seq)
);

-- Readiness of each live change, refreshed from GitHub. Works like `check_runs`: intake
-- bumps desired_version; a worker queries GitHub and records the result. `recheck_at`
-- re-polls changes even without events, since GitHub does not promise to deliver check
-- events from other Apps to Sheltie.
CREATE TABLE readiness (
    change_id        BIGINT PRIMARY KEY REFERENCES changes (id) ON DELETE CASCADE,
    desired_version  BIGINT NOT NULL DEFAULT 1,
    synced_version   BIGINT NOT NULL DEFAULT 0,
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    recheck_at       TIMESTAMPTZ,
    last_error       TEXT,
    approved         BOOLEAN,
    checks_green     BOOLEAN,
    draft            BOOLEAN,
    checked_at       TIMESTAMPTZ,
    CHECK (synced_version <= desired_version)
);

CREATE INDEX readiness_due_idx ON readiness (next_attempt_at)
    WHERE synced_version < desired_version;
CREATE INDEX readiness_recheck_idx ON readiness (recheck_at);

-- The latest shadow prediction per change, and every change of prediction.
CREATE TABLE predictions (
    change_id   BIGINT PRIMARY KEY REFERENCES changes (id) ON DELETE CASCADE,
    epoch_at    TIMESTAMPTZ NOT NULL,
    decision    TEXT NOT NULL CHECK (decision IN (
                    'land_now', 'reverify', 'merge_engine', 'speculative_chain', 'group_batch')),
    -- The scheduler's decision as JSON, with peers as PR numbers.
    detail      JSONB NOT NULL,
    -- One line for the check run.
    summary     TEXT NOT NULL
);

CREATE TABLE prediction_log (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    change_id   BIGINT NOT NULL REFERENCES changes (id) ON DELETE CASCADE,
    epoch_at    TIMESTAMPTZ NOT NULL,
    decision    TEXT NOT NULL,
    detail      JSONB NOT NULL,
    summary     TEXT NOT NULL
);

CREATE INDEX prediction_log_change_idx ON prediction_log (change_id, id);
