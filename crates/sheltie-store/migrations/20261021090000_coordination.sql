-- M7: coordination. Issues, predicted footprints and leases.

-- The latest build graph of each repository's default branch, uploaded by the adapter.
CREATE TABLE graph_snapshots (
    repo_id      BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    commit_sha   git_oid NOT NULL,
    snapshot     JSONB NOT NULL,
    uploaded_by  TEXT NOT NULL,
    uploaded_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Issues as last seen (title and description are needed for prediction).
CREATE TABLE issues (
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number      BIGINT NOT NULL CHECK (number > 0),
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    open        BOOLEAN NOT NULL,
    -- The forge's update time; orders out-of-order deliveries.
    event_at    TIMESTAMPTZ NOT NULL,
    -- Whether Sheltie has commented the predicted footprint.
    noted       BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY (repo_id, number)
);

-- Issues a pull request's description closes ("Fixes #12").
CREATE TABLE pr_closes (
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    pr_number     BIGINT NOT NULL,
    issue_number  BIGINT NOT NULL,
    PRIMARY KEY (repo_id, pr_number, issue_number)
);
CREATE INDEX pr_closes_issue_idx ON pr_closes (repo_id, issue_number);

-- Repository-scoped tokens for agents (REST and MCP). Only the SHA-256 is stored.
CREATE TABLE api_tokens (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    token_hash  BYTEA NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at  TIMESTAMPTZ
);

-- A lease reserves an issue's predicted footprint for one holder.
CREATE TABLE leases (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    issue_number     BIGINT NOT NULL,
    holder           TEXT NOT NULL,
    writes           TEXT[] NOT NULL,
    reads            TEXT[] NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL,
    released_at      TIMESTAMPTZ,
    released_reason  TEXT
);
-- At most one active lease per issue.
CREATE UNIQUE INDEX leases_active_issue_idx ON leases (repo_id, issue_number) WHERE released_at IS NULL;

-- Claims by assignment, processed by the coordinator; kept while blocked.
CREATE TABLE lease_waits (
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    issue_number  BIGINT NOT NULL,
    holder        TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Whether the issue was labelled and commented as blocked.
    notified      BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY (repo_id, issue_number, holder)
);

-- Comments and labels to post on issues; a worker sends them (outbox).
CREATE TABLE issue_notices (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    issue_number     BIGINT NOT NULL,
    comment          TEXT,
    add_label        TEXT,
    remove_label     TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at          TIMESTAMPTZ,
    attempts         INTEGER NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    leased_until     TIMESTAMPTZ
);
CREATE INDEX issue_notices_due_idx ON issue_notices (next_attempt_at) WHERE sent_at IS NULL;
