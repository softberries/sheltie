-- M1: GitHub App intake. Installations, webhook deliveries, PR events and the check-run outbox.

CREATE TABLE installations (
    -- GitHub's installation id.
    id             BIGINT PRIMARY KEY CHECK (id > 0),
    account_login  TEXT NOT NULL,
    suspended_at   TIMESTAMPTZ,
    deleted_at     TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- NULL when the App no longer has access to the repository.
ALTER TABLE repositories ADD COLUMN installation_id BIGINT REFERENCES installations (id);
ALTER TABLE repositories ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- The diff hash is computed with the footprint (M2), after intake.
ALTER TABLE changes ALTER COLUMN diff_hash DROP NOT NULL;

-- Forge time (pull_request.updated_at) of the newest lifecycle event applied to this change.
-- Webhooks can arrive out of order; older lifecycle events for the PR are recorded but ignored.
ALTER TABLE changes ADD COLUMN event_at TIMESTAMPTZ;

-- Every processed delivery; the primary key drops GitHub redeliveries.
CREATE TABLE webhook_deliveries (
    delivery_id  TEXT PRIMARY KEY CHECK (length(delivery_id) BETWEEN 1 AND 100),
    event        TEXT NOT NULL CHECK (length(event) BETWEEN 1 AND 100),
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Append-only log of pull request events (sheltie_core::intake::PrEventKind).
CREATE TABLE pr_events (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    delivery_id  TEXT NOT NULL REFERENCES webhook_deliveries (delivery_id),
    repo_id      BIGINT NOT NULL REFERENCES repositories (id),
    pr_number    BIGINT NOT NULL CHECK (pr_number > 0),
    head_sha     git_oid NOT NULL,
    base_sha     git_oid NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN (
                     'opened', 'synchronized', 'reopened', 'closed', 'merged',
                     'ready_for_review', 'converted_to_draft',
                     'approved', 'changes_requested', 'review_dismissed')),
    actor        TEXT NOT NULL,
    -- Forge time of the event (pull_request.updated_at).
    event_at     TIMESTAMPTZ NOT NULL,
    -- False when a newer lifecycle event had already been applied.
    applied      BOOLEAN NOT NULL,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX pr_events_pr_idx ON pr_events (repo_id, pr_number, id);

-- Outbox for the "Sheltie" check run of each change. Intake bumps desired_version; a worker
-- posts the change's current state to GitHub and records synced_version. Rows are claimed by
-- pushing next_attempt_at forward (a lease), so several replicas can run the worker.
CREATE TABLE check_runs (
    change_id        BIGINT PRIMARY KEY REFERENCES changes (id) ON DELETE CASCADE,
    github_id        BIGINT CHECK (github_id > 0),
    desired_version  BIGINT NOT NULL DEFAULT 1,
    synced_version   BIGINT NOT NULL DEFAULT 0,
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error       TEXT,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (synced_version <= desired_version)
);

CREATE INDEX check_runs_due_idx ON check_runs (next_attempt_at)
    WHERE synced_version < desired_version;
