-- M6: intents and replays (tier 4).

-- The intent from the pull request description, or why it was rejected.
ALTER TABLE changes ADD COLUMN intent JSONB;
ALTER TABLE changes ADD COLUMN intent_error TEXT;

-- Description edits update the intent.
ALTER TABLE pr_events DROP CONSTRAINT pr_events_kind_check;
ALTER TABLE pr_events ADD CONSTRAINT pr_events_kind_check CHECK (kind IN (
    'opened', 'synchronized', 'reopened', 'closed', 'merged',
    'ready_for_review', 'converted_to_draft',
    'approved', 'changes_requested', 'review_dismissed', 'edited'));

-- One replay attempt: requested from the runner, then reported.
CREATE TABLE replay_runs (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    change_id      BIGINT NOT NULL REFERENCES changes (id) ON DELETE CASCADE,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id),
    tier           TEXT NOT NULL CHECK (tier IN ('replay_hunks', 'replay_full')),
    onto_sha       git_oid NOT NULL,
    requested_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    reported_at    TIMESTAMPTZ,
    result_sha     git_oid,
    checks_passed  BOOLEAN,
    similarity     REAL CHECK (similarity >= 0 AND similarity <= 1),
    -- Estimated by the agent; used for the daily budget.
    cost_usd       DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (cost_usd >= 0),
    turns          INTEGER,
    error          TEXT,
    -- Acted upon by the committer.
    handled        BOOLEAN NOT NULL DEFAULT false
);

CREATE INDEX replay_runs_change_idx ON replay_runs (change_id);
CREATE INDEX replay_runs_repo_day_idx ON replay_runs (repo_id, requested_at);
