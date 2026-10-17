-- M5: speculative chains and group-tested batches.

-- A group of changes built as prefixes on one head, in priority order.
CREATE TABLE spec_groups (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id),
    members     BIGINT[] NOT NULL CHECK (cardinality(members) >= 1),
    onto_sha    git_oid NOT NULL,
    -- The merge commit after each member, so landed members get their own merge commit.
    member_shas TEXT[] NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One prefix build of a group: the first `length` members merged onto the head.
CREATE TABLE spec_builds (
    group_id     BIGINT NOT NULL REFERENCES spec_groups (id) ON DELETE CASCADE,
    length       INTEGER NOT NULL CHECK (length >= 1),
    branch       TEXT NOT NULL,
    sha          git_oid NOT NULL,
    PRIMARY KEY (group_id, length)
);
