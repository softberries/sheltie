-- M9: the read-only dashboard.

-- Pull request titles, for display.
ALTER TABLE changes ADD COLUMN title TEXT;

-- Dashboard sessions after GitHub sign-in. Only the SHA-256 of the session id is stored; the
-- repositories are those the user could access when signing in.
CREATE TABLE dashboard_sessions (
    id_hash     BYTEA PRIMARY KEY CHECK (length(id_hash) = 32),
    login       TEXT NOT NULL,
    repo_ids    BIGINT[] NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);
CREATE INDEX dashboard_sessions_expiry_idx ON dashboard_sessions (expires_at);
