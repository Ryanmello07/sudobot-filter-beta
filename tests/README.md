# Tests

## Unit + integration suite (default, always green)

```sh
cargo test
```

Covers the inline `#[cfg(test)]` modules in `src/` plus `tests/simulations.rs`,
including runtime config overrides (`guild_overrides` store round-trips, override
TOML validation and merge order, manager authorization rules) and the
mod-log channel override.

## Regression tests for audit findings (failing on purpose)

Bug-exposing tests are written against the *desired* behavior and marked
`#[ignore = "<finding-id>: ..."]`, so the default run stays green while the bugs are
documented as executable specifications. They currently FAIL — that is the point.
Do not run them en masse; run one at a time to confirm a finding before fixing it:

```sh
cargo test -- --ignored c1_omitted_established_kick_ban_stay_disabled   # C1 config.rs
cargo test -- --ignored c1_shipped_config_keeps_established_kick_ban_disabled
cargo test -- --ignored s3_trailing_dot_domain_matches_blocked          # S3 rules.rs
cargo test -- --ignored s8_invite_allowlist_is_case_sensitive           # S8 rules.rs
cargo test -- --ignored m5_reprocessing_does_not_overwrite_action_taken # M5 pipeline.rs
cargo test -- --ignored s2_jev_cache_key_includes_author_context        # S2 pipeline.rs
cargo test -- --ignored m1_local_only_max_score_reaches_quarantine_delete_threshold # M1 fusion.rs
cargo test -- --ignored m4_jev_only_other_abuse_is_labeled_abuse        # M4 fusion.rs
cargo test -- --ignored s5_submit_to_full_queue_returns_within_bounds   # S5 dispatch.rs
```

## Simulations (wiremock, no network, must pass)

```sh
cargo test --test simulations
```

Provider outage, flaky-provider retry, 429 quota exhaustion, a 200-message raid
(prints throughput), and a table-driven evasion corpus documenting current
normalize/rules coverage.

## Live API quality checks (real quota, opt-in)

```sh
cp .env.example .env   # fill in real keys
cargo test --test live_api -- --ignored --nocapture
```

Each live test skips (passes) when its provider key is missing. See the doc comment
at the top of `tests/live_api.rs`. These consume real API quota.
