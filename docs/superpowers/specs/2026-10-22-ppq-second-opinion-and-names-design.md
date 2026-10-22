# Paxmod for messages only; Jev + Perplexity via ppq for names, second opinion and fallback

## Intent

Paxmod keeps a running record of each conversation (`user_id` + `context_id`) and reads new messages in that context. Anything that is not a chat message pollutes that record and can confuse it. Today the join screen sends `"username display_name"` to Paxmod with the member's `user_id`, so names land in that person's Paxmod history. Separately, when Paxmod is skipped, over cap, or failing, nothing can delete or time out a message: Jev is report-only (weight 0.45) and cannot cross the 0.50 delete line alone.

The owner wants:

- Paxmod to see **only chat messages** (and the existing context feed), nothing else.
- Two stateless one-message-at-a-time models, both served by **ppq**, used together for everything Paxmod should not see, and as the safety net when Paxmod is unavailable:
  - Jev (`typesafe/jev-1.13` on ppq), replacing the direct Typesafe API.
  - Perplexity (`perplexity/pplx-decider-v1.2` on ppq), new to the bot.

## Goals

1. Name checks at join go to Jev and Perplexity (in parallel). Paxmod is never called for names.
2. Jev and Perplexity both run in parallel, always, on all content the bot scans (messages including OCR text, and names), because they are cheap. This includes every message the plan sends to Paxmod.
3. **Acting source order.** Only Paxmod can drive delete + timeout while it responds. If Paxmod produced no result for a message (over cap, circuit open, failed, timed out), Perplexity becomes the acting source on its own. If Perplexity also has no result, Jev becomes the acting source on its own. If none responded, local rules stand alone, as today.
4. Models that are not the acting source are report-only: they show in the mod-log evidence and can raise a flag, never a delete or timeout.
5. The Typesafe API client and its env vars go away; ppq is the single base URL and key for both models.

## Non-goals / anti-goals

- No change to what Paxmod receives for messages: same text, `user_id`, `context_id = channel_id`, same context feed, same OCR gating.
- While Paxmod responds (even degraded), Jev and Perplexity must never be able to cause a delete or timeout, alone or together, even if both say block with full confidence and Paxmod scored low. (Today Jev can tip a borderline Paxmod score over the line through noisy-or; that goes away.)
- No automatic kick or ban, ever. Name checks still only raise the member's tier and post a notice (existing behavior).
- Do not touch `config.toml` values owned by the operator beyond adding the new weight key with the same default as `w_jev`; do not edit `.env` or `data/`.
- No Paxmod prompt/policy changes (that is edited in the Paxmod console).

## Decisions made by the owner

- ppq replaces the Typesafe API for Jev. Jev and Perplexity run at the same time, always, on all scanned content (cheap).
- Paxmod exclusively for messages.
- Only Paxmod can auto-mute while it responds.
- Fallback when Paxmod does not respond: Perplexity first, then Jev.

## Calls made by me (change if you disagree)

- **One acting source per verdict, others report-only.** Acting source = Paxmod, else Perplexity, else Jev, else local alone. The acting source's risk enters fusion at weight 1.0 (the `w_paxmod` pattern). Every other model's risk is report-only: its evidence is shown, and it may raise the score only up to the flag rung (below 0.50), never to delete or timeout. Report-only models do not stack with each other (the larger one counts). No new thresholds; the ladder is unchanged.
- **Config weights.** The acting source's weight is read from config: `w_paxmod` (existing), new `w_pplx`, and existing `w_jev`. In the repo's `config.toml` all three are set to 1.0 (so `w_jev` changes from 0.45 to 1.0 and its comment is rewritten for the new rule); the code defaults for `w_pplx` and `w_jev` stay 0.9, like `w_paxmod`'s code default. A weight is only applied when that source is the acting source. Add `fusion.w_pplx` to the same `/guard config` allowlists and displays that list `fusion.w_jev` today. The live server's `config.toml` carries its own copy of these values; updating it is part of the deploy step (needs the owner's go-ahead), not the build.
- **Report-only models never enter the noisy-or.** Let `base` = noisy-or of the local term and the acting source's term (as today, minus the report-only models). Report-only models contribute a separate term `r = min(max(their risks), CAP)` with `CAP = min(0.45, delete_threshold_of_tier - 0.01)`, and the final score is `max(base, r)`. So a report-only model can lift a message to the flag rung but can never combine with local rules to cross delete or timeout, and a guild override with a low delete threshold cannot break that guarantee. (Flag rungs above the cap in non-live code defaults simply mean report-only models do not flag there.)
- **Local rules** keep their current role and weights and still combine with the acting source via noisy-or. When the only live source is local, it stands alone as today.
- **`agreeing_sources` counts only local and the acting source**, never report-only models, so the kick/ban "needs two sources" rule (disabled in the live config, enabled in code defaults) cannot be satisfied by report-only models. The verdict `label` may still use any model's class (for example a Jev or Perplexity `scam_phishing` class gives the Scam label) because it is display information; the builder must confirm the label does not influence which action is chosen, and if it does, restrict it to the acting source.
- **Policy file.** Commit `policy.txt` at the repo root, seeded from `decision_bench/paxmod_prompt_candidate.txt` (the 797-character policy now live in Paxmod). Path overridable via `POLICY_FILE`. If the file is missing or empty at startup the bot logs a warning and runs both models without the `policy` question; the parser treats the `policy` answer as optional (the other five answers stay strictly required).
- **Both models get the same questions and the same parser as Jev today** (class, dm_lure, crypto_giveaway, evasion, bot_like; verified live on both models with identical response shape), plus **one new choice question `policy`** (`block` / `allow`) whose instructions are the owner's Paxmod policy text, because that is the form Perplexity was benchmarked on (F1 97.9 with the old policy, not yet measured with the new 797-character policy). `p(block)` feeds the spam risk as `max(existing, p_block)`. The policy text lives in a repo file read at startup (path via env `POLICY_FILE`, default `policy.txt`), so it can be updated without a rebuild; it is an operator-edited copy and may drift from the Paxmod console text.
- **Name checks** use the same call with `state.message = "<username> <display_name>"` and a separate choice question `name_bad` (block / allow: "this username or display name impersonates staff or support, carries an invite or link, advertises, or is a scam or giveaway bait") in place of `policy`. A join reason is added if either model gives spam risk >= 0.8 (today's Jev rule). The cheap local rules in `handle_join` stay.
- **Model names are pinned in env** (`JEV_MODEL=typesafe/jev-1.13`, `PPLX_MODEL=perplexity/pplx-decider-v1.2`). The `jev-latest` alias does not exist on ppq.
- Both calls hit ppq `POST {base}/decisions`; base default `https://api.ppq.ai/v1`. New env: `PPQ_API_KEY`, `PPQ_BASE_URL`, `PPLX_MODEL`, `PPLX_DAILY_CAP` (default 2000, same as Jev). `TYPESAFE_*` removed from code, `.env.example` and docs.
- Each model gets its own budget, circuit breaker, usage row (`pplx`), result cache (same TTL and key rule as the Jev cache), and `Coverage` field. Old stored `verdict_json` rows must still deserialize (`pplx` coverage defaults).
- Incident rows record the Perplexity result in a new nullable column (`pplx_json`) via a new migration, like `jev_json`.
- The mod-log evidence shows a `pplx:` line next to the `jev:` line; for a fallback timeout the audit-log reason uses the existing no-Paxmod form (`Automated Temporary Mute: <label> <score>`).
- ppq rate-limits at 600 requests/min per key, shared by both models. At two calls per scanned message this is fine for current traffic; a 429 maps to the existing Quota handling and trips the same breaker.

## Constraints

- Rust codebase in `repo/`, existing patterns: provider traits in `ports.rs`, clients in `src/providers/`, fakes in `pipeline.rs` tests, wiremock for client tests.
- Commit on `main`; deploy to `root@74.50.11.62` only when the owner says so. The server needs `PPQ_API_KEY` (and the other new vars) added to its `.env` by the owner before the new binary starts; do not write any key into the repo.
- Existing tests must keep passing (168 lib + 5 simulations at last count); new behavior needs tests, in particular:
  - the Paxmod fake sees no call from `handle_join`;
  - Paxmod fails: Perplexity high -> delete+timeout even with Jev saying clean; Perplexity low and Jev high -> flag only;
  - Paxmod and Perplexity both unavailable: Jev high -> delete+timeout;
  - Paxmod responded with a low score + Perplexity and Jev both at full confidence -> never delete or timeout (flag at most);
  - both models are called for every provider-scanned message, and for names;
  - old `verdict_json` still parses.

## Verification before calling it done

- Offline: `cargo test`, `cargo build --bins` clean.
- Live smoke test of both ppq models through the new client with a few real strings (scam, ordinary chat, scam name, normal name); budget counted in cents.
- Benchmark check with `decision_bench/` data (2,394 + 400 rows): report false-block and miss rates for Perplexity alone, and for Jev alone, using the `policy` question. Perplexity alone was 2.2% false blocks on the old policy, so this number matters: it is what users see during a Paxmod outage. Report the numbers to the owner before deploying; do not tune thresholds silently.

## Left to the builder

- Exact wording of the `name_bad` and `policy` question instructions and criteria labels.
- Where the fallback/second-opinion distinction lives (extra field on `Inputs` vs a parameter to `fuse`).
- Module layout: one generic ppq decisions client vs two thin wrappers; trait naming (`Classifier` is reused for both).
- Migration numbering and column details; test fixture layout.
