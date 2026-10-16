-- M2: footprints uploaded by CI adapters.
--
-- Keyed by repository and head commit rather than by change: the upload can arrive before
-- the webhook that creates the change, and one head can belong to several pull requests.
-- The original `footprints` table (keyed by change) was never written; it is replaced.

DROP TABLE footprints;

CREATE TABLE footprint_reports (
    repo_id       BIGINT NOT NULL REFERENCES repositories (id),
    head_sha      git_oid NOT NULL,
    -- Merge base of the PR's base and head; the tree it was computed against.
    base_sha      git_oid NOT NULL,
    base_tree     git_oid NOT NULL,
    diff_hash     TEXT NOT NULL CHECK (length(diff_hash) BETWEEN 1 AND 512),
    adapter       TEXT NOT NULL CHECK (length(adapter) BETWEEN 1 AND 512),
    writes        TEXT[] NOT NULL,
    reads         TEXT[] NOT NULL,
    tests         TEXT[] NOT NULL,
    leaky         BOOLEAN NOT NULL,
    leak_reasons  TEXT[] NOT NULL,
    -- GitHub Actions run that uploaded it, from the OIDC token.
    uploaded_by   TEXT NOT NULL,
    uploaded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, head_sha, base_tree)
);

-- Footprints are reused for any change with the same base tree and diff.
CREATE INDEX footprint_reports_diff_idx ON footprint_reports (repo_id, base_tree, diff_hash);
