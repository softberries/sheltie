-- Tokens a replay's agent used, as reported by the runner (NULL when it did not say).
ALTER TABLE replay_runs
    ADD COLUMN input_tokens       BIGINT CHECK (input_tokens >= 0),
    ADD COLUMN output_tokens      BIGINT CHECK (output_tokens >= 0),
    ADD COLUMN cache_read_tokens  BIGINT CHECK (cache_read_tokens >= 0),
    ADD COLUMN cache_write_tokens BIGINT CHECK (cache_write_tokens >= 0);

-- `[replay] daily_budget_usd` from `.sheltie/merge.toml`, for the dashboard (NULL: replay off).
ALTER TABLE repositories
    ADD COLUMN replay_daily_budget_usd DOUBLE PRECISION CHECK (replay_daily_budget_usd > 0);
