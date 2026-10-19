-- What a replay's prompt was built from, the tests found in the pull request and how they ran,
-- and whether any acceptance test checked the result (NULL until reported, or from clients
-- before 0.6).
ALTER TABLE replay_runs ADD COLUMN details JSONB;
