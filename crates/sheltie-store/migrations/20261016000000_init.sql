-- Initial schema: the core records from docs/design.md ("Data model and APIs").
--
-- Enum-like columns are TEXT with CHECK constraints. Their allowed values are the text forms of
-- the matching sheltie-core enums (ChangeState, ResolutionTier, GuaranteeTier); tests in
-- sheltie-store fail if the two drift apart.

-- Git object ids: SHA-1 (40) or SHA-256 (64), lowercase hex. Mirrors sheltie_core::CommitSha.
CREATE DOMAIN git_oid AS TEXT
    CHECK (VALUE ~ '^[0-9a-f]{40}([0-9a-f]{24})?$');

CREATE TABLE repositories (
    -- GitHub's numeric repository id; stable across renames and transfers.
    id          BIGINT PRIMARY KEY CHECK (id > 0),
    -- "owner/name" as last seen; informational, may change.
    full_name   TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE changes (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id),
    pr_number   BIGINT NOT NULL CHECK (pr_number > 0),
    head_sha    git_oid NOT NULL,
    base_sha    git_oid NOT NULL,
    diff_hash   TEXT NOT NULL,
    intent_ref  TEXT,
    state       TEXT NOT NULL CHECK (state IN (
                    'pending', 'ready', 'merging', 'verifying',
                    'committable', 'landed', 'returned', 'closed')),
    risk        REAL NOT NULL DEFAULT 0 CHECK (risk >= 0 AND risk <= 1),
    priority    BIGINT NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, pr_number, head_sha)
);

-- The scheduler reads live changes per repository every epoch.
CREATE INDEX changes_live_idx ON changes (repo_id, state)
    WHERE state NOT IN ('landed', 'closed');

CREATE TABLE footprints (
    change_id    BIGINT NOT NULL REFERENCES changes (id) ON DELETE CASCADE,
    base_tree    git_oid NOT NULL,
    writes       TEXT[] NOT NULL,
    reads        TEXT[] NOT NULL,
    tests        TEXT[] NOT NULL,
    leaky        BOOLEAN NOT NULL,
    computed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (change_id, base_tree)
);

CREATE TABLE verifications (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tree          git_oid NOT NULL,
    targets       TEXT[] NOT NULL,
    outcome       TEXT NOT NULL CHECK (outcome IN ('pass', 'fail', 'flaky')),
    -- Set only when outcome = 'fail'.
    failed_tests  TEXT[] NOT NULL DEFAULT '{}',
    runner        TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (outcome = 'fail' OR cardinality(failed_tests) = 0)
);

-- Verifications are reused by tree.
CREATE INDEX verifications_tree_idx ON verifications (tree);

CREATE TABLE resolutions (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tier        TEXT NOT NULL CHECK (tier IN (
                    'commute', 'structural', 'regenerate', 'declared',
                    'replay_hunks', 'replay_full', 'human')),
    files       TEXT[] NOT NULL,
    similarity  REAL CHECK (similarity >= 0 AND similarity <= 1),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Only intent replay produces a similarity score.
    CHECK (similarity IS NULL OR tier IN ('replay_hunks', 'replay_full'))
);

-- The changes a resolution combined, in application order.
CREATE TABLE resolution_changes (
    resolution_id  BIGINT NOT NULL REFERENCES resolutions (id) ON DELETE CASCADE,
    position       INTEGER NOT NULL CHECK (position >= 0),
    change_id      BIGINT NOT NULL REFERENCES changes (id),
    PRIMARY KEY (resolution_id, position),
    UNIQUE (resolution_id, change_id)
);

-- One row per successful compare-and-swap on a repository's target branch.
-- Inserting (repo_id, seq) where seq = previous + 1 is the CAS: a concurrent committer that
-- read the same head fails on the primary key.
CREATE TABLE head_events (
    repo_id     BIGINT NOT NULL REFERENCES repositories (id),
    seq         BIGINT NOT NULL CHECK (seq >= 0),
    commit_sha  git_oid NOT NULL,
    change_id   BIGINT NOT NULL REFERENCES changes (id),
    tier        TEXT NOT NULL CHECK (tier IN ('footprint', 'speculation', 'sweep')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, seq)
);
