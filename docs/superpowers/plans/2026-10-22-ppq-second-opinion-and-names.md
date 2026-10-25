# ppq Jev + Perplexity: names, second opinion, Paxmod fallback — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paxmod sees only chat messages; Jev and Perplexity (both via ppq) scan all content and names in parallel; only Paxmod can delete/time out while it responds, otherwise Perplexity acts alone, then Jev.

**Architecture:** One ppq `/decisions` client (`PpqClient`) serves both models (model name is a constructor argument). `fuse` picks one *acting source* (Paxmod, else Perplexity, else Jev) whose risk enters at its config weight; the other models are report-only, capped below the tier's lowest action rung and never entered into the noisy-or. `handle_join` stops calling Paxmod and calls both models.

**Tech Stack:** Rust, tokio, reqwest, sqlx (SQLite migrations), wiremock for client tests; benchmark script in Python 3.9 stdlib under `decision_bench/` (outside the repo).

**Spec:** `docs/superpowers/specs/2026-10-22-ppq-second-opinion-and-names-design.md`

## Global Constraints

- Endpoint for both models: `POST {PPQ_BASE_URL}/decisions`, body `{"state": <JevState>, "model": <model>, "questions": {...}}`, bearer auth with `PPQ_API_KEY`. Defaults: `PPQ_BASE_URL=https://api.ppq.ai/v1`, `JEV_MODEL=typesafe/jev-1.13`, `PPLX_MODEL=perplexity/pplx-decider-v1.2`, `JEV_DAILY_CAP=2000`, `PPLX_DAILY_CAP=2000`, `POLICY_FILE=policy.txt`. `TYPESAFE_*` removed everywhere.
- Paxmod receives only chat messages (same text, `user_id`, `context_id = channel_id`, same context feed, same OCR gating). Never names.
- Acting source order: Paxmod (any `Some` result, even degraded) → Perplexity → Jev → local alone. Acting weights come from config (`w_paxmod`, new `w_pplx`, `w_jev`); code defaults 0.9 each; repo `config.toml` sets all three to 1.0.
- Report-only models never enter the noisy-or. Their term is `min(max(their risks), CAP)` with `CAP = min(0.45, lowest_action_rung_of_tier - 0.01)`; final spam/abuse score = `max(base, term)`.
- `agreeing_sources` counts only local and the acting source.
- **Corroboration gate (owner priority: a wrong action on a legitimate member is much worse than a missed spam):** any automatic action above `Flag` requires `Verdict.corroborated`. Corroborated = some source other than the acting one has spam-or-abuse risk >= 0.5 (local counts only at score >= 0.5); when no model responded and local rules stand alone, treated as corroborated. Not corroborated → `decide` returns at most `Action::Flag`, and the verdict evidence gets `note: no second source agreed; flag only`.
- Name check: join reason if either model's spam risk >= 0.8. Name checks never kick or ban (existing).
- Policy text from `POLICY_FILE`; missing/empty → warn and run without the `policy` question (parser treats `policy` / `name_bad` answers as optional; the other five answers stay strictly required).
- Both models: own budget, circuit breaker, usage row (`pplx`), result cache (same TTL/key rule as Jev's), `Coverage.pplx` (serde default), `incidents.pplx_json` (nullable, migration `0004`).
- Commit on `main`. Do NOT deploy, do NOT edit `.env` or `data/`, never write a key into the repo. Edit `config.toml` only for the `[defaults.fusion]` keys and their comment.
- Gates: `scripts/check.sh` (fmt, clippy `-D warnings`, test) passes at the end of every code task.

## Review Focus

1. The two live incidents, replayed: #77 (Paxmod 1.0, Jev legitimate, no local rule: flag only) and #78 (Paxmod 0.0, local `first_message_link` 0.35, Jev promo 0.97: flag at most). Also: Paxmod responds clean/flagged/degraded while Perplexity and Jev both say block at full confidence and a local rule fires: no delete, no timeout. (Tasks 4, 5)
2. Perplexity errors (timeout, 429, malformed) while Paxmod is also down: Jev must act; Perplexity erroring while Paxmod responds changes nothing. (Task 5)
3. `policy.txt` missing, empty, whitespace-only, or invalid UTF-8: bot still starts, requests omit the `policy` question. (Task 3)
4. Blank or emoji-only names, names/messages longer than the truncation limits with multi-byte characters: no panic, blank names skip both model calls. (Tasks 2, 6)
5. Existing databases and stored `verdict_json` rows without `pplx`: migration applies, old rows still deserialize. (Tasks 3, 5)

## Execution order and release plan

Run Task 1, then **Task 4 first**: it fixes the two live false positives using only what the bot already has (Paxmod + Jev + local) and is releasable on its own as a hotfix (owner go-ahead needed to deploy). Then Tasks 2, 3, 5, 6, 7 (ppq, Perplexity, names) as the second release, and Task 8 (policy mention carve-out) in parallel whenever the owner is ready to paste into the Paxmod console.

---

### Task 1: Fallback benchmark on the new policy (outside the repo)

Independent of all code tasks; run first so the owner has the numbers early. Nothing in `repo/` changes.

**Files:**
- Create: `decision_bench/fallback_bench.py` (reuses `decision_bench/core.py` `Pacer`, `Cache`, `request`; key from `decision_bench/.env` `PPQ_API_KEY`)
- Output: `decision_bench/results/fallback_report.md`

**Interfaces:**
- Produces: the report with, for each of `perplexity/pplx-decider-v1.2` and `typesafe/jev-1.13`, false-block rate on clean rows and miss rate on scam rows, over `data/dataset.jsonl` (2,394) and `data/val.jsonl` (400), per group; plus a name sample result.

- [ ] **Step 1: Write the script.** Request shape must match the bot: `POST https://api.ppq.ai/v1/decisions`, `state = {"message": text}` (image rows: `"\n[image text] " + text`), questions = the bot's five (copy from `src/providers/jev.rs::questions()`) plus `policy`: `{"type":"choice","instructions": <contents of repo/policy.txt or decision_bench/paxmod_prompt_candidate.txt> + "\n\nDecide whether this message should be blocked or allowed under the policy above.","criteria":{"block":"Violates the policy above","allow":"Does not violate the policy above"}}`. A row is "blocked" when `answers.policy.probabilities.block >= 0.5`. Pace at 570 requests/min via `core.Pacer`; cache per (model, row id).
- [ ] **Step 2: Add a name sample inside the script:** 20 scam/impersonation names (e.g. "FREE NITRO giveaway", "Support Team | DM me", "discord.gg/abc123", "Mod Support") and 20 ordinary names (e.g. "alice_gaming", "Marco", "ネコ", "xX_Sniper_Xx", "SupportiveSam"), asked with the name question: instructions `"Decide whether this Discord username or display name should be blocked: it impersonates staff or support, advertises, carries an invite or link, or is a scam or giveaway lure."`, criteria `{"block":"Should be blocked","allow":"Fine"}`; report block rate on each half.
- [ ] **Step 3: Run it:** `cd decision_bench && python3 fallback_bench.py` → writes `results/fallback_report.md`; Expected: no failed calls (retry once if some), report contains both models.
- [ ] **Step 4: Report the numbers to the owner** (false-block %, miss %, name results). This is informational; nothing blocks on it, but the owner decides before deploy whether Perplexity-alone acting is acceptable. No commit (outside the repo).

---

### Task 2: `PpqClient`, question sets, `policy_block`

**Files:**
- Modify: `src/providers/jev.rs` (rename `JevClient` → `PpqClient`, endpoint, questions, parse), `src/model.rs` (`JevResult`), `src/ports.rs` (`Classifier`), `src/fusion.rs` (`jev_risks` only), `src/pipeline.rs` (call sites + fake only)
- Test: unit tests in `src/providers/jev.rs`, `src/fusion.rs`

**Interfaces:**
- Produces:
  - `pub enum Ask { Message, Name }` (`Copy`, `Debug`, `PartialEq`) in `providers/jev.rs`
  - `pub fn questions(ask: Ask, policy: Option<&str>) -> serde_json::Value` — the five existing questions always; plus `policy` (Message, only when `policy` is `Some`) or `name_bad` (Name). Choice questions use criteria keys `block` / `allow`.
  - `pub struct PpqClient`; `pub fn PpqClient::new(base: &str, key: Secret, model: &str, policy: Option<String>, timeout: Duration) -> Self`; `pub async fn assess(&self, state: &JevState, ask: Ask) -> Result<JevResult, ProviderError>` posting to `{base}/decisions`
  - `JevResult.policy_block: Option<f64>` (`#[serde(default)]`) = `p(block)` of the `policy` or `name_bad` answer, if present
  - `Classifier::assess(&self, state: &JevState, ask: Ask)`; `impl Classifier for PpqClient`
  - `jev_risks` spam term becomes `max(existing, policy_block.unwrap_or(0.0))`
- Consumes: nothing.

- [ ] **Step 1: Write failing tests** in `providers/jev.rs`:
  - `message_request_includes_policy_question`: wiremock on `POST /decisions`, `body_partial_json({"model":"m","questions":{"policy":{"type":"choice"},"dm_lure":{"type":"noul"}}})`; client built with `Some("POLICY TEXT".into())`; assert the received body's `questions.policy.instructions` contains `"POLICY TEXT"`.
  - `no_policy_omits_question`: client with `None`; assert received body's `questions` has no `policy` key.
  - `name_request_uses_name_bad_not_policy`: `Ask::Name` with a policy set; body has `name_bad`, no `policy`.
  - `parse_reads_policy_block`: response with `"policy":{"type":"choice","choice":"block","probabilities":{"block":0.93,"allow":0.07}}` → `policy_block == Some(0.93)`; with `"choice":"block"` and no probabilities → `Some(1.0)`; `"choice":"allow"` and no probabilities → `Some(0.0)`; no `policy`/`name_bad` key → `None`.
  - Update the existing tests (`request_shape_and_parse`, `http_errors`, `timeout`) to the new path/constructor/`Ask::Message`; `schema_mismatches_are_malformed` must still reject a response missing any of the five original answers.
  - In `fusion.rs`: `policy_block_feeds_spam_risk`: `JevResult { class: "legitimate", class_probs: {"legitimate": 1.0}, policy_block: Some(0.95), ..Default::default() }` → `jev_risks(&r).0 >= 0.95`.
- [ ] **Step 2: Run** `cargo test --lib providers::jev fusion::tests::policy_block` — Expected: FAIL (types missing).
- [ ] **Step 3: Implement** the interfaces above. Question wording is fixed: `policy` instructions = `<policy> + "\n\nDecide whether this message should be blocked or allowed under the policy above."`, criteria `{"block":"Violates the policy above","allow":"Does not violate the policy above"}`; `name_bad` instructions = `"Decide whether this Discord username or display name should be blocked: it impersonates staff or support, advertises, carries an invite or link, or is a scam or giveaway lure."`, criteria `{"block":"Should be blocked","allow":"Fine"}`. Update `Classifier` and mechanical callers (`pipeline.rs` passes `Ask::Message` for messages and `Ask::Name` in `handle_join`; `FakeJev::assess` takes and ignores the new parameter) so the crate compiles. Text truncation stays char-based (`trunc`).
- [ ] **Step 4: Run** `cargo test --lib` — Expected: PASS (pipeline/other tests unaffected apart from the signature).
- [ ] **Step 5: Commit** `git commit -m "feat: PpqClient for ppq /decisions with policy and name questions"`

---

### Task 3: Env, config, budgets, wiring, policy file

**Files:**
- Modify: `src/config.rs` (`Env`, `FusionConfig`, allowlists/display), `src/budget.rs` (`Provider::Pplx`), `src/model.rs` (`Coverage.pplx`), `src/pipeline.rs` (`Deps.pplx`, `Budgets.pplx`, test harness only), `src/main.rs`, `src/bin/replay.rs`, `tests/simulations.rs`, `tests/live_api.rs`, `tests/live_adversarial.rs`, `.env.example`, `config.toml`
- Create: `policy.txt` (repo root; contents = `decision_bench/paxmod_prompt_candidate.txt`, 797 chars, no trailing newline issues)
- Test: unit tests in `src/config.rs`, `src/model.rs`

**Interfaces:**
- Produces:
  - `Env` fields: remove `typesafe_key`, `typesafe_base`; add `ppq_key: Secret`, `ppq_base: String`, `pplx_model: String`, `pplx_cap: u64`, `policy_file: String`; `jev_model` default becomes `"typesafe/jev-1.13"`
  - `pub fn load_policy(path: &str) -> Option<String>` in `config.rs`: `None` if the file is missing, unreadable, invalid UTF-8, or trims to empty; otherwise the trimmed text
  - `Provider::Pplx` with `name() == "pplx"`
  - `Coverage.pplx: Cov` with `#[serde(default)]` on the struct so old JSON still parses
  - `Deps.pplx: Arc<dyn Classifier>`, `Budgets.pplx: Arc<Budget>`; main preloads `Provider::Pplx` usage like the others
- Consumes: `PpqClient::new` from Task 2.

- [ ] **Step 1: Write failing tests:** `load_policy_missing_empty_and_trimmed` (missing path → `None`; file with `"  \n"` → `None`; file with `" text \n"` → `Some("text")`; bytes `[0xff,0xfe]` → `None`) using `tempfile` or `std::env::temp_dir()` paths; in `model.rs`: `old_coverage_json_without_pplx_deserializes` (`{"paxmod":"Ran","jev":"Ran","ocr":"Skipped"}` → ok, `pplx == Cov::Skipped`).
- [ ] **Step 2: Run** `cargo test --lib config::tests model::tests` — Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces. `main.rs`: build one `PpqClient` per model (`env.jev_model`, `env.pplx_model`) with `load_policy(&env.policy_file)` (log a warning when `None`), replace the `TYPESAFE_API_KEY` placeholder warning with `PPQ_API_KEY`. `replay.rs`, `tests/simulations.rs` (wiremock paths now `/decisions`; add a third mock for Perplexity), `tests/live_*.rs` (use `ppq_key`/`ppq_base`, `#[ignore]` kept): mechanical updates so everything compiles. `pipeline.rs` test harness: add `harness_full(cfg, pax, pplx, jev, enforce, roll, sink)`; existing `harness(...)` calls it with `FakeJev::clean()` for Perplexity; `H` gains `pplx: Arc<FakeJev>`. Pipeline logic is not changed here.
- [ ] **Step 4: `.env.example`:** replace the Typesafe block with `PPQ_API_KEY`, `PPQ_BASE_URL=https://api.ppq.ai/v1`, `JEV_MODEL=typesafe/jev-1.13`, `JEV_DAILY_CAP=2000`, `PPLX_MODEL=perplexity/pplx-decider-v1.2`, `PPLX_DAILY_CAP=2000`, `POLICY_FILE=policy.txt`. `config.toml` `[defaults.fusion]`: `w_pplx = 1.0`, `w_jev = 1.0`, and replace the comment block with the new rule (acting source = Paxmod, else Perplexity, else Jev; everything else report-only, capped below the action rungs).
- [ ] **Step 5: Run** `scripts/check.sh` — Expected: all gates pass.
- [ ] **Step 6: Commit** `git commit -m "feat: ppq env/config wiring, w_pplx weight, policy.txt"`

---

### Task 4: Fusion — acting source, report-only cap, corroboration gate (hotfix; run right after Task 1)

Releasable on its own: the pipeline passes `pplx: None` until Task 5, so this fixes incidents #77/#78 with only Paxmod + Jev + local rules.

**Files:**
- Modify: `src/fusion.rs`, `src/policy.rs`, `src/model.rs` (`Verdict`), `src/config.rs` (`FusionConfig.w_pplx`), `src/pipeline.rs` (`fuse` call site; pipeline tests)
- Test: unit tests in fusion/policy/config; update existing `fusion.rs` and `policy.rs` tests whose expectations assumed `w_jev` entered the noisy-or or that construct `Verdict { .. }` directly (grep `Verdict {`)

**Interfaces:**
- Produces:
  - `FusionConfig.w_pplx: f64` (default 0.9), added next to every existing `w_jev` occurrence in `config.rs` (default impl, `/guard config` key lists and setters ~lines 360, 402, 418, 535, display ~587). Do not edit `config.toml` in this task.
  - `Verdict.corroborated: bool` with `#[serde(default)]`
  - `policy::decide` returns at most `Action::Flag` when `!v.corroborated` (and the flag rung is reached); behavior when `corroborated` is unchanged
  - `Inputs.pplx: Option<&'a JevResult>`
  - `pub fn fuse(inp: &Inputs, cfg: &FusionConfig, clean_below: f64, report_cap: f64) -> Verdict`
  - `pub const REPORT_CAP_MAX: f64 = 0.45;` and `pub fn report_cap(tier: Tier, l: &LadderConfig) -> f64` in `policy.rs`: `min(REPORT_CAP_MAX, min(delete, timeout, kick, ban of that tier's rungs that are Some) - 0.01)`; `REPORT_CAP_MAX` if the tier has no rungs set; `Trusted` → `REPORT_CAP_MAX`
  - Evidence: `pplx:class=… dm=… crypto=… evasion=… bot=… policy=…` line (same format as the `jev:` line, plus `policy=` when present); the label may still use any model's `scam_phishing`/`nsfw_lure` class
- Consumes: nothing from other tasks (`JevResult.policy_block` from Task 2 is not needed here).

- [ ] **Step 1: Write failing tests.** Gate tests first:
  - In `fusion.rs`: `corroborated_when_another_source_agrees`: Paxmod Spam 1.0 + Jev spam → `corroborated`; Paxmod Spam 1.0 + Jev legitimate (`legitimate` 1.0) → not corroborated; Paxmod 1.0 + local score 0.6 → corroborated; Paxmod 1.0 + local score 0.35 → not corroborated; local alone (no model) → corroborated (outage behavior unchanged); `FusionConfig::default().w_pplx == 0.9`; the uncorroborated verdict's evidence contains `"note: no second source agreed; flag only"`.
  - In `policy.rs`: `uncorroborated_never_exceeds_flag`: `verdict(0.99, Label::Spam, 1)` with `corroborated = false` in every tier → `Action::Flag`; with `corroborated = true` → unchanged expectations (existing tests set it true).
  - In `pipeline.rs` (enforce mode, quarantine user, `roll = 0.99`): `incident_77_replay_flags_only`: `FakePax::spam_flagged(1.0, "tagging a user to initiate contact")`, `FakeJev::clean()`, text `"its working or not. <@1308810777939148800>"` → `h.sink.empty()`, incident `taken == "flag"`; `incident_78_replay_is_not_actioned`: `FakePax::spam(0.0)`, Jev `promo_solicitation` 0.97 (`class_probs {"promo_solicitation": 0.97, "legitimate": 0.01}`, `class_confidence 0.96`), message with a link in a first message (so `first_message_link` fires) → `h.sink.empty()`; `paxmod_flag_with_jev_agreement_still_acts`: `FakePax::spam_flagged(1.0, "x")` + `FakeJev::spam()` → sink has `delete` and `timeout`.
  - Then the fusion tests (all with `FusionConfig::default()`, `clean_below = 0.4`, `report_cap = 0.45` unless stated; `spam_model()` = `JevResult { class: "spam", class_probs: {"spam": 1.0, "legitimate": 0.0}, ..Default::default() }`):
  - `paxmod_acts_models_report_only`: Paxmod Spam 0.1 + `spam_model()` for both pplx and jev → `score == 0.45` (±1e-9), `label != Clean`, `agreeing_sources == 0`.
  - `local_plus_report_only_cannot_cross_delete`: local score 0.6 + Paxmod 0.0 + both models spam → `score < 0.5`.
  - `pplx_acts_when_paxmod_missing`: pplx spam, jev spam → `score == 0.9`; pplx spam, jev clean → `0.9`; pplx clean (`legitimate` 1.0), jev spam → `score == 0.45`.
  - `jev_acts_when_paxmod_and_pplx_missing`: jev spam alone → `score == 0.9`.
  - `report_cap_parameter_is_respected`: Paxmod 0.0, models spam, `report_cap = 0.2` → `score == 0.2`.
  - `local_alone_stands_alone`: unchanged existing behavior (weight 1.0 when no provider source is present).
  - In `policy.rs`: `report_cap_follows_lowest_rung`: ladder with quarantine `delete = 0.50, timeout = 0.50` → `0.45`; with `delete = 0.40` → `0.39`; tier with no rungs set → `0.45`.
- [ ] **Step 2: Run** `cargo test --lib fusion::tests policy::tests pipeline::tests::incident_7` — Expected: FAIL.
- [ ] **Step 3: Implement.** Compute `corroborated` in `fuse` per the Global Constraints definition, store it on `Verdict`, add the evidence note when false, and gate in `decide`. Acting = first `Some` of paxmod, pplx, jev. Acting source: `weight * risk` into the spam and abuse term lists (existing Paxmod/Jev code per source), counts toward `agreeing` at risk >= 0.5, and Paxmod keeps its category/reason/ban evidence. Local term uses `w_local` when any provider source is present (even report-only ones, since `agreeing`/outage semantics key off *responses*), else 1.0. Report-only sources: evidence line only, and `spam_report = min(max(their spam risks), report_cap)`, `abuse_report` likewise; final `spam_score = max(noisy_or(spam_terms), spam_report)`, same for abuse. Update the module doc comment.
  - Label check (spec asked the builder to confirm): `policy::decide` uses the label only for `Clean` (no action) and for `Spam | Scam` gating kick/ban, which also needs `agreeing_sources >= 2`; since report-only models no longer count toward `agreeing_sources`, a label taken from their class cannot escalate any action. Add `report_only_scam_class_cannot_enable_kick_or_ban` in `policy.rs` or `fusion.rs`: Paxmod 0.0, both models `scam_phishing` spam 1.0 → `agreeing_sources == 0` and `decide(...)` with kick/ban enabled returns no action above `Flag`.
- [ ] **Step 4: Fix existing tests** in `fusion.rs` that assumed the old multi-source noisy-or (`sources_combine_noisy_or` etc.): rewrite expectations to the new rule rather than loosening them. Update the call sites of `fuse` (`pipeline.rs`: pass `report_cap(tier, &gc.ladder)`; `pplx: None` for now).
- [ ] **Step 5: Run** `scripts/check.sh` — Expected: all gates pass.
- [ ] **Step 6: Commit** `git commit -m "fix: corroboration gate and report-only cap; models can no longer tip Paxmod over the action line"`
- [ ] **Step 7: Hotfix release note for the owner:** this task can be deployed alone (sync `src/`, rebuild, restart; no config or `.env` changes needed). With the server's current `w_jev = 0.45`, Jev cannot act on its own, which is the conservative behavior wanted.

---

### Task 5: Pipeline — run Perplexity and Jev on every scanned message

**Files:**
- Modify: `src/pipeline.rs`, `src/store.rs`, `src/discord/modlog.rs` (only the sample coverage string if a test pins it)
- Create: `migrations/0004_pplx_json.sql` (`ALTER TABLE incidents ADD COLUMN pplx_json TEXT;`)
- Test: pipeline unit tests, one store test

**Interfaces:**
- Produces:
  - `enum Model { Jev, Pplx }` (private) and `async fn run_classifier(&self, model: Model, guild: u64, text: &str, state: JevState, ask: Ask, prio: Priority, now: DateTime<Utc>, cache_ctx: &str) -> (Option<JevResult>, Cov, Vec<String>)` replacing `run_jev`; selects dep, budget, cache (`pplx_cache: TtlCache<String, JevResult>` next to `jev_cache`), breaker label (`"Jev"` / `"Perplexity"`), usage name (`"jev"` / `"pplx"`), failure prefix, and over-cap announcement text
  - `NewIncident.pplx_json: Option<String>`; `insert_incident` persists it (add to INSERT column list and the `ON CONFLICT` update list)
- Consumes: `fuse` / `Inputs.pplx` (Task 4), `Deps.pplx`, `Budgets.pplx`, `Coverage.pplx` (Task 3).

- [ ] **Step 1: Write failing tests** (harness: enforce mode, `roll = 0.99`, `new_user(7)` quarantine message `"hello there"`; build fakes with `FakeJev::spam()` / `clean()`):
  - `both_models_called_once_per_scanned_message`: `h.pplx.n() == 1 && h.jev.n() == 1 && h.pax.n() == 1`.
  - `paxmod_down_pplx_acts_alone`: `FakePax::failing()`, pplx spam, jev clean → sink has `delete` and `timeout` entries; incident `taken == "delete+timeout"`.
  - `paxmod_down_pplx_clean_jev_spam_flags_only`: sink empty; incident `taken == "flag"`.
  - `paxmod_and_pplx_down_jev_acts`: pplx `Err(ProviderError::Server(503))` (also a `Quota` variant), jev spam → `delete` + `timeout`.
  - `paxmod_responds_models_block_never_acts`: `FakePax::spam(0.0)`, both models spam, message text that triggers a local rule (e.g. a duplicate or mass-mention fixture used elsewhere in this file) → `sink.empty()`, outcome is flag-or-none, never `delete`/`timeout`.
  - `pplx_results_are_cached_for_crossposted_text`: mirrors `jev_results_are_cached_for_crossposted_text` with `h.pplx.n() == 1`.
  - `coverage_reports_pplx`: `verdict.coverage.pplx == Cov::Ran`; with failing pplx → `Cov::Failed`.
  - Store: `incident_persists_pplx_json` (insert with `pplx_json: Some("{}")`, read the column back with a raw query in the test module).
  - Update tests that relied on Jev acting during outages, or on Jev tipping Paxmod, to the new semantics.
- [ ] **Step 2: Run** `cargo test --lib pipeline::tests store::tests` — Expected: FAIL.
- [ ] **Step 3: Implement.** In `handle`, replace the `tokio::join!(paxmod, jev)` with a three-way join that builds the same `JevState` for both models and uses `cache_ctx` identical to today's Jev one; set `coverage.pplx`; pass `pplx: pplx.as_ref()` into `Inputs`; serialize `pplx_json` into `NewIncident`; add the `pplx` result to the "clean scan counts" condition exactly as `jev == Cov::Ran` is used today. `timeout_reason` call unchanged (a fallback timeout uses the existing `<label> <score>` form).
- [ ] **Step 4: Run** `scripts/check.sh` — Expected: all gates pass.
- [ ] **Step 5: Commit** `git commit -m "feat: run Perplexity and Jev on every scanned message; pplx_json on incidents"`

---

### Task 6: Join screening without Paxmod

**Files:**
- Modify: `src/pipeline.rs` (`handle_join` and its tests)
- Test: pipeline unit tests

**Interfaces:**
- Consumes: `run_classifier` (Task 5), `Ask::Name`.
- Produces: join reasons `"Perplexity flagged name ({:.2})"` and `"Jev flagged name ({:.2})"` when that model's spam risk (`jev_risks(..).0`) >= 0.8.

- [ ] **Step 1: Write failing tests:**
  - Update the two existing join tests: assert `h.pax.n() == 0` for both the flagged and the ordinary join (replacing the "Paxmod saw no context_id" assertion).
  - `join_flags_when_either_model_flags_name`: pplx spam/jev clean → notice contains `"Perplexity flagged name"`; jev spam/pplx clean → notice contains `"Jev flagged name"`; both clean + ordinary name → no notice.
  - `join_with_blank_names_skips_models`: `username: ""`, `display_name: ""` → `h.pplx.n() == 0 && h.jev.n() == 0`, returns `false` unless a local reason fires.
  - `join_models_get_name_state`: use a fake that records the `JevState.message`; assert it equals `"alice Alice"` and that `account_age_days`/`has_default_avatar` are set from the profile.
  - Update the usage-recording test (`context_feed_and_join_screening_record_usage`) to expect `pplx` and `jev` rows and no `paxmod` row from the join.
- [ ] **Step 2: Run** `cargo test --lib pipeline::tests join` — Expected: FAIL.
- [ ] **Step 3: Implement.** Delete the Paxmod call and its result handling in `handle_join`. Skip both model calls when `names.trim()` is empty. Otherwise call both through `run_classifier` concurrently with `Ask::Name`, `Priority::Low`, a per-model cache key of `"name:" + normalized names` (names are not message text; do not share the message cache entries), and `state.message = names.trim()`, `minutes_in_guild: Some(0)`. Keep the local name rules and the "raise tier + notice, never act" behavior.
- [ ] **Step 4: Run** `scripts/check.sh` — Expected: all gates pass.
- [ ] **Step 5: Commit** `git commit -m "feat: join name screening via Perplexity + Jev; Paxmod never sees names"`

---

### Task 7: Final verification and handoff

**Files:**
- Modify: `tests/README.md` (live-test env vars), `docs/superpowers/specs/2026-10-22-ppq-second-opinion-and-names-design.md` (append "Implemented" with commit range)

- [ ] **Step 1: Full gates:** `scripts/check.sh` and `cargo build --release --bins` — Expected: pass, no warnings.
- [ ] **Step 2: Live smoke test (needs a ppq key in the shell env, not in the repo):** `PPQ_API_KEY=… cargo test --test live_api -- --ignored --nocapture live_jev` — Expected: both models return parseable results for a scam and a benign string; cost shown in cents. If no key is available to the agent, ask the owner to run it.
- [ ] **Step 3: grep check:** `grep -rn -i "typesafe" --include="*.rs" --include="*.toml" --include="*.example" --include="*.md" . --exclude-dir=target --exclude-dir=.git --exclude-dir=superpowers` shows no matches except model names (`typesafe/jev-…`).
- [ ] **Step 4: Commit** `git commit -m "docs: live-test env vars, implementation note"` and report to the owner: task commits, test counts, the Task 1 benchmark numbers.

### Task 8: Policy mention carve-out (outside the repo; the owner pastes into the Paxmod console)

Fixes the Paxmod side of incident #77. Independent of the code tasks; `repo/policy.txt` (Task 3) is only updated at the end if it exists.

**Files:**
- Create: `decision_bench/data/mentions.jsonl` (same row format as `data/dataset.jsonl`: `id,text,label,group,category,kind,difficulty`), `decision_bench/prompts/cand_mention_<n>.txt`
- Modify: `decision_bench/paxmod_bench.py` (add `--data` and `--out` arguments, defaults unchanged so existing runs behave the same)

**Interfaces:**
- Produces: a candidate policy ≤ 800 characters that Paxmod does not flag on ordinary mentions, plus before/after numbers.

- [ ] **Step 1: Build the mention set.** At least 60 clean rows (ordinary chat with raw `<@1308810777939148800>`-style mentions leading, trailing and mid-sentence: thanks, replies, "ask <@id>", "its working or not. <@id>", mod pings, "good morning <@id>") and at least 25 scam rows that use mentions (`FREE SUPPORT SERVER 2026 (NO SCAM) DM 👉 <@id>`, "need help? DM <@id>", obfuscated invite + mention ping, "add me on telegram @handle <@id>", profile-link redirects with a ping). Start from the lists in `decision_bench/mention_probe.py`; write the rest by hand, varied in tone and length; no template swaps.
- [ ] **Step 2: Baseline on real Paxmod with the current policy:** `python3 paxmod_bench.py --data data/mentions.jsonl --out results/paxmod_mentions_base.jsonl`; record false-block count on clean rows and misses on scam rows (the 2/18 from the probe is the expected ballpark).
- [ ] **Step 3: Write 3–5 candidates** as rewrites of `prompts/cand_rw_4.txt` that add a short carve-out (ordinary `<@id>` mentions, replies and pings in conversation are not contact pushes; "DM me", "DM <@id>", "add me", and profile/handle pointers still are), staying ≤ 800 characters (check with `len(open(p,encoding="utf-8").read().strip())`). Screen them on the proxy panel with `prompt_opt.py` (`val`) plus the mention set, keep the best one or two.
- [ ] **Step 4: Owner pastes the best candidate** into the Paxmod console. Then run, against real Paxmod: the mention set, `data/val.jsonl`, and the full `data/dataset.jsonl`. Acceptance: at most 1 false block in the clean mention rows, no scam row from the mention set newly missed, and the main dataset still at 0 false blocks with scams caught ≥ 99%. If it fails, the old policy is in `decision_bench/paxmod_prompt_candidate.txt` (restore it) and the next candidate is tried.
- [ ] **Step 5: If it passes,** copy the text to `repo/policy.txt` (when Task 3 has landed) and `decision_bench/paxmod_prompt_candidate.txt`; commit `policy.txt` only.

---

## Not part of this plan (owner go-ahead required)

Deploy: sync `src/`, `migrations/`, `policy.txt`, `Cargo.*` to `/root/sudobot_filter`; on the server edit `.env` (`PPQ_API_KEY` already added by the owner; set `JEV_MODEL=typesafe/jev-1.13`, add `PPLX_MODEL`, `PPLX_DAILY_CAP`, `POLICY_FILE`, remove `TYPESAFE_*`) and `config.toml` `[defaults.fusion]` (`w_pplx = 1.0`, `w_jev = 1.0`); `cargo build --release`; back up the binary; restart `sudobot-filter.service`; watch the log for `decision:` lines and ppq errors. Rollback: restore the backed-up binary and the old `.env`/`config.toml`.
