# OCR Scope, Mod Buttons, Mark-Safe Unmute, Timeout Reason — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop OCR false positives by scanning images only for genuinely new members, and fix the mod-log buttons, Mark safe, and the timeout audit-log reason.

**Architecture:** Small, local edits to the existing Rust bot. OCR gating is a pure predicate in `risk.rs` applied to the detection plan in `pipeline.rs`. Button/gate changes live in `discord/modlog.rs` and `discord/interact.rs`. The `ActionSink` port gains a `reason` on `timeout` and a new `untimeout`, with the pipeline owning the reason text and the "lift bot timeout" decision so both are unit-testable with the existing fakes.

**Tech Stack:** Rust, serenity 0.12.5, tokio, sqlx (sqlite), `cargo test`.

**Spec:** Approved in chat on 2026-10-19 (no separate design doc; the approved description is the "Requirements" section below).

## Requirements (approved)

1. **OCR scope:** OCR runs only for members whose *time-based* tier is Quarantine, or Fresh because they are genuinely new (joined < `fresh_days_in_guild` days ago OR account < `fresh_account_days` days old). A member who is Fresh *only* because of the `graduation_clean_messages` rule (old account, long in server, few clean messages) gets **no OCR**. Established and Trusted members never get OCR (this removes the existing "weak image message" OCR for Established). Text scanning/sampling is unchanged.
2. **Buttons:** When the bot already timed the member out (`taken == "delete+timeout"`), the mod-log embed shows **Mark safe, Kick, Ban** (no Timeout button). Kick and Ban must work on incidents the bot already `delete+timeout`'d. Incidents with other outcomes (flag, delete, observe, refused, failed) keep all four buttons.
3. **Mark safe:** Still allowlists the member and clears the incident's strike. Additionally lifts the bot's timeout when the incident's `action_taken` is `delete+timeout`. If the lift fails (member left, etc.) the allowlist/strike changes still apply and the reply says the timeout could not be lifted.
4. **Timeout reason:** The bot's own timeouts use the audit-log reason `Automated Temporary Mute: <paxmod reason>`, single line, at most 512 characters. If Paxmod gave no usable reason, use `Automated Temporary Mute: <label> <score:.2>`.

## Global Constraints

- Do NOT edit `config.toml`, `.env`, or anything under `data/`.
- Do NOT push to origin and do NOT touch the server. Commit locally on `main` only.
- Audit-log reason cap: 512 characters (Discord limit).
- Reason prefix is exactly `Automated Temporary Mute: `.
- Test command for every task: `cargo test --lib --test simulations` (run from `repo/`). It must be green before each commit. Also run `cargo build --bins` once at the end of Tasks 3 and 4 (they touch `src/bin/replay.rs`).
- Follow existing style: short doc comments only where behavior is non-obvious; match surrounding test style (`#[test]` in-file `mod tests`, `#[tokio::test]` for pipeline).

## Review Focus

Input classes the spec implies but the requirements don't spell out; each has a pinning test in the owning task:

1. Old account with `joined_at == None` and no history → no OCR (Task 1).
2. "Sleeper" member: account 2000d, 365d in server, 0 clean scans (Fresh tier) posting an image → no OCR (Task 1). This is the Yip2 case from the incident screenshot.
3. Paxmod reason containing newlines, runs of spaces, or longer than 512 chars, or multi-byte characters → one line, ≤ 512 chars, never panics on a char boundary (Task 3).
4. Stale mod-log embeds that still carry a Timeout button, clicked on a `delete+timeout` incident → polite refusal ("already timed out"), not an API call (Task 2).
5. Mark safe on an incident that was only flagged/deleted, observe-only, or where the bot kicked/banned → no untimeout call; and Mark safe when the untimeout API call fails → allowlist still applied, reply explains (Task 4).

---

### Task 1: OCR only for genuinely new members

**Files:**
- Modify: `src/risk.rs` (add `ocr_eligible` + tests)
- Modify: `src/pipeline.rs:246` area (apply the gate) and the test `established_plain_image_is_not_ocrd` (~line 1176)

**Interfaces:**
- Produces: `pub fn ocr_eligible(p: &MemberProfile, cfg: &TierConfig, now: DateTime<Utc>) -> bool` in `src/risk.rs`. True iff `base_tier(p, cfg, now)` is `Tier::Quarantine` or `Tier::Fresh` (time-based only; history is deliberately ignored; Trusted → false).

- [ ] **Step 1: Write failing unit tests in `src/risk.rs` `mod tests`**, named `ocr_eligible_only_for_time_young_members`, using the existing `profile(account_age, in_guild)` helper, `TierConfig::default()` and `now()`:
  - `profile(2000d, 5min)` → true (Quarantine)
  - `profile(1000d, 3d)` → true (Fresh by days in guild)
  - `profile(10d, 100d)` → true (Fresh by young account)
  - `profile(2000d, 365d)` → **false** (sleeper/old; would be Fresh by history rule but not by time)
  - `profile(1000d, 7d)` → false (exactly at the 7-day boundary is no longer Fresh)
  - `joined_at = None` with `profile(1000d, ..)` → false
  - `is_trusted = true` on `profile(1d, 1min)` → false

- [ ] **Step 2: Run** `cargo test --lib ocr_eligible` — expected: compile failure (function not defined).

- [ ] **Step 3: Implement `ocr_eligible`** next to `base_tier` in `src/risk.rs` as `matches!(base_tier(p, cfg, now), Tier::Quarantine | Tier::Fresh)`, with a one-line doc comment explaining that OCR is reserved for newly joined accounts because images read out of context cause false positives.

- [ ] **Step 4: Apply the gate in `Pipeline::handle`** (`src/pipeline.rs`): after `let plan = detection_plan(...)` make it `let mut plan` and set `plan.ocr = plan.ocr && ocr_eligible(&profile, &gc.tiers, now);` (import `ocr_eligible` in the existing `use crate::risk::{...}`). Use `profile` (the one with the allowlist applied), not `m.profile`. Do this before `self.maybe_feed(...)` so the decision log line reports the final plan.

- [ ] **Step 5: Update/add pipeline tests** in `src/pipeline.rs` `mod tests`:
  - Replace `established_plain_image_is_not_ocrd` with `established_members_are_never_ocrd`: after `seed_clean(&h, 8)`, an image-only message (`""` text) from `old_user(8)` and a plain-text-with-image message both leave `h.ocr.calls` at 0.
  - Add `sleeper_without_history_is_not_ocrd` (Review Focus 2): no `seed_clean`; `old_user(8)` with an image-only message → `h.ocr.calls == 0` and `o.tier == Tier::Fresh`.
  - Add `time_young_fresh_member_is_ocrd`: `profile(9, 24 * 10, 60 * 24 * 2)` (10-day account, 2 days in guild, Fresh) with an image-only message → `h.ocr.calls == 1`.
  - Existing `image_only_scam_is_caught_via_ocr_and_text_reaches_providers` (Quarantine) must still pass unchanged.

- [ ] **Step 6: Run** `cargo test --lib --test simulations` — expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src/risk.rs src/pipeline.rs
git commit -m "feat: OCR only for time-young members (Quarantine / genuinely new Fresh)"
```

---

### Task 2: Mod-log buttons — hide Timeout after auto-timeout, allow Kick/Ban

**Files:**
- Modify: `src/discord/modlog.rs` (`incident_buttons`, add test)
- Modify: `src/discord/interact.rs` (`manual_action_gate` and its tests)

**Interfaces:**
- `incident_buttons(r: &IncidentReport) -> Vec<CreateButton>`: unchanged signature. Omits the `timeout:{id}` button iff `r.taken == "delete+timeout"`.
- `manual_action_gate(kind, admin, perms, action_taken, outcome, can_act)`: unchanged signature. New behavior: `ban`/`kick` are allowed when `action_taken == "delete+timeout"`; still refused (`"This incident was already actioned."`) when `action_taken` is `"ban"` or `"kick"` or `outcome` starts with `"manual "`. `kind == "timeout"` with `action_taken == "delete+timeout"` returns `Err("This member was already timed out by the bot.".into())`.

- [ ] **Step 1: Write failing tests**
  - In `modlog.rs` tests: `buttons_hide_timeout_after_auto_timeout`. Serialize each button with `serde_json::to_value`, collect `custom_id`s. For `report("delete+timeout", false)` expect `["safe:123", "kick:123", "ban:123"]`; for `report("flag", false)`, `report("delete", false)` and `report("would-have", true)` (the pipeline sets `taken = "would-have"` in observe mode) expect all four in order `safe, timeout, kick, ban`.
  - In `interact.rs` `manual_action_gate_branches`: change the existing `("timeout", ..., "delete+timeout", "open")` assertion to expect an error containing `"already timed out"`; add assertions that `manual_action_gate("ban", true, Permissions::empty(), "delete+timeout", "open", &ok)` and the same for `"kick"` are `Ok("ban")` / `Ok("kick")`; keep the "already actioned" assertions for `action_taken == "ban"` and for `outcome == "manual ban by 9"`.

- [ ] **Step 2: Run** `cargo test --lib buttons_hide_timeout manual_action_gate` (run as two invocations if needed) — expected: FAIL.

- [ ] **Step 3: Implement** both changes. In `incident_buttons`, build the vec, filtering out the timeout button when `r.taken == "delete+timeout"`. In `manual_action_gate`, replace `matches!(action_taken, "ban" | "kick" | "delete+timeout")` with `matches!(action_taken, "ban" | "kick")` and add the `kind == "timeout" && action_taken == "delete+timeout"` refusal *after* the already-actioned check and *before* the permission check.

- [ ] **Step 4: Run** `cargo test --lib --test simulations` — expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/discord/modlog.rs src/discord/interact.rs
git commit -m "fix: mods can Kick/Ban after an automatic timeout; hide redundant Timeout button"
```

---

### Task 3: Timeout audit-log reason carries the Paxmod reason

**Files:**
- Modify: `src/ports.rs` (`ActionSink::timeout` gains `reason`; add `untimeout`)
- Modify: `src/discord/actions.rs` (implement both)
- Modify: `src/pipeline.rs` (`timeout_reason`, `execute`, `FakeSink`, `FakePax`, tests)
- Modify: `tests/simulations.rs:57` and `src/bin/replay.rs:81` (fake sinks: new signature + `untimeout`)

**Interfaces:**
- Produces in `src/ports.rs`:
  - `async fn timeout(&self, guild: u64, user: u64, minutes: u32, reason: &str) -> anyhow::Result<()>`
  - `async fn untimeout(&self, guild: u64, user: u64, reason: &str) -> anyhow::Result<()>`
- Produces in `src/pipeline.rs`: `pub fn timeout_reason(pax_reason: Option<&str>, label: &str, score: f64) -> String` (module-level free function). Rules: prefix `"Automated Temporary Mute: "`; detail = the Paxmod reason with all whitespace runs (including newlines) collapsed to single spaces and trimmed; if `pax_reason` is `None` or the collapsed detail is empty, detail = `format!("{label} {score:.2}")`; the full string is cut to at most 512 *characters* (not bytes) and, when cut, ends with `…` (still ≤ 512 chars total).
- `Pipeline::execute` gains a trailing parameter `timeout_reason: &str` and passes it to `s.timeout(g, m.author_id, l.timeout_minutes, timeout_reason)`.
- `DiscordSink::untimeout` uses `EditMember::new().enable_communication().audit_log_reason(reason)`.

- [ ] **Step 1: Write failing unit tests for `timeout_reason`** in `pipeline.rs` `mod tests`, test name `timeout_reason_formats_and_caps`:
  - `timeout_reason(Some("crypto drainer tactic"), "spam", 0.81)` == `"Automated Temporary Mute: crypto drainer tactic"`
  - `timeout_reason(None, "spam", 0.81)` == `"Automated Temporary Mute: spam 0.81"`
  - `timeout_reason(Some("   \n  "), "abuse", 0.5)` == `"Automated Temporary Mute: abuse 0.50"`
  - `timeout_reason(Some("line one\nline two   spaced"), "spam", 0.9)` == `"Automated Temporary Mute: line one line two spaced"`
  - Long input `"é".repeat(600)` → result has `chars().count() == 512`, ends with `'…'`, starts with the prefix (no panic on multi-byte).
  - Input that makes the result exactly 512 chars is not cut and has no `…`.

- [ ] **Step 2: Write a failing pipeline test** `auto_timeout_uses_paxmod_reason_in_audit_log`: add `FakePax::spam_flagged(score: f64, reason: &str)` returning a `PaxmodResult` with `categories: [("Spam", score)]`, `flagged: true`, `reason: Some(reason.into())`. Handle a Quarantine spam message in enforce mode (like `quarantine_spam_is_deleted_timed_out_and_swept_in_enforce`) and assert `h.sink.has("timeout 7 60 | Automated Temporary Mute: <reason>")`. Update `FakeSink::timeout` to log `format!("timeout {u} {mins} | {reason}")` (existing `has("timeout 7 60")` assertions are prefix matches and keep working). Add a second assertion path: with plain `FakePax::spam(0.8)` (not flagged, no reason) the log line ends with `"Automated Temporary Mute: spam 0.80"` — use the label/score from `o.verdict` rather than hardcoding if the fused score differs.

- [ ] **Step 3: Run** `cargo test --lib timeout_reason auto_timeout_uses` — expected: compile failure / FAIL.

- [ ] **Step 4: Implement**
  1. `ports.rs`: change `timeout` signature, add `untimeout`.
  2. `actions.rs`: `timeout` passes `reason` to `audit_log_reason`; add `untimeout` (reuse the existing style; no new helper needed).
  3. `pipeline.rs`: add `timeout_reason`. In `handle`, just before `self.execute(...)`, compute `let reason = timeout_reason(pax.as_ref().filter(|p| p.flagged).and_then(|p| p.reason.as_deref()), verdict.label.name(), verdict.score);` and pass `&reason` to `execute` (only flagged Paxmod results contribute their reason, matching `fusion.rs`).
  4. Update the three fake sinks (`pipeline.rs` tests, `tests/simulations.rs`, `src/bin/replay.rs`) to the new `timeout` signature and add a trivial `untimeout` (`FakeSink` in `pipeline.rs` logs `format!("untimeout {u}")` and gets a `fail_untimeout: bool` field defaulting to false that makes it return `Err(anyhow::anyhow!("unknown member"))` — Task 4 uses it).

- [ ] **Step 5: Run** `cargo test --lib --test simulations` and `cargo build --bins` — expected: all pass / builds.

- [ ] **Step 6: Commit**

```bash
git add src/ports.rs src/discord/actions.rs src/pipeline.rs tests/simulations.rs src/bin/replay.rs
git commit -m "feat: automatic timeouts record 'Automated Temporary Mute: <paxmod reason>' in the audit log"
```

---

### Task 4: Mark safe lifts the bot's timeout

**Files:**
- Modify: `src/pipeline.rs` (new `Release` enum + `lift_bot_timeout`, tests)
- Modify: `src/discord/interact.rs` (`"safe"` branch of `component`, reply text helper + test)

**Interfaces:**
- Consumes: `ActionSink::untimeout` and `FakeSink.fail_untimeout` from Task 3; `store::IncidentRow` (fields `id`, `guild_id`, `user_id`, `action_taken`).
- Produces in `src/pipeline.rs`:
  - `pub enum Release { NotTimedOut, Lifted, Failed(String) }` (derive `Debug, Clone, PartialEq, Eq`)
  - `impl Pipeline { pub async fn lift_bot_timeout(&self, inc: &IncidentRow, moderator: u64) -> Release }` — returns `NotTimedOut` without calling the sink unless `inc.action_taken == "delete+timeout"`; otherwise calls `untimeout(inc.guild_id, inc.user_id, &format!("Timeout lifted: marked safe by moderator {moderator} (incident #{})", inc.id))` and returns `Lifted` or `Failed(err.to_string())`.
- Produces in `src/discord/interact.rs`: `fn safe_reply(r: &Release) -> String` returning:
  - `NotTimedOut` → `"Member added to the allowlist and the strike from this incident cleared."` (the current text)
  - `Lifted` → `"Member added to the allowlist, the strike from this incident cleared, and the timeout lifted."`
  - `Failed(e)` → `format!("Member added to the allowlist and the strike cleared, but the timeout could not be lifted ({e}). They may have left, or the timeout already expired.")`

- [ ] **Step 1: Write failing pipeline tests** in `pipeline.rs` `mod tests`:
  - `mark_safe_lifts_bot_timeout`: enforce-mode Quarantine spam (as in Task 3's test) → `o.taken == "delete+timeout"`; fetch the row with `h.p.store().incident(o.incident_id.unwrap()).await.unwrap().unwrap()`; `lift_bot_timeout(&row, 99)` == `Release::Lifted`; `h.sink.has("untimeout 7")`.
  - `mark_safe_does_not_untimeout_other_incidents` (Review Focus 5): rows with `action_taken` of `"flag"`, `"would-have"`, `"delete"`, `"kick"`, `"ban"` (obtain by setting up a flag-only incident, or by constructing an `IncidentRow` literal with those values — the struct fields are all public) → `NotTimedOut` and `h.sink.count("untimeout") == 0`.
  - `mark_safe_reports_failed_unmute`: `FakeSink { fail_untimeout: true, ..Default::default() }`; after an auto-timeout, `lift_bot_timeout` returns `Release::Failed(_)` containing `"unknown member"`.

- [ ] **Step 2: Write failing test** `safe_reply_texts` in `interact.rs` tests covering the three variants (exact strings above; the `Failed` case checks `contains("could not be lifted")` and the error text).

- [ ] **Step 3: Run** `cargo test --lib mark_safe safe_reply` — expected: compile failure / FAIL.

- [ ] **Step 4: Implement** `Release`, `lift_bot_timeout`, and `safe_reply`. In the `"safe"` branch of `interact::component`: after `set_allowlisted`, `clear_strikes_for_incident`, and `set_outcome` (keep these running first so they are never skipped), call `p.lift_bot_timeout(&inc, c.user.id.get()).await` and reply with `safe_reply(&release)`. Also keep the Manage Server permission gate unchanged. Update the stale comment at the end of the match only if it becomes wrong.

- [ ] **Step 5: Run** `cargo test --lib --test simulations` and `cargo build --bins` — expected: all pass / builds.

- [ ] **Step 6: Commit**

```bash
git add src/pipeline.rs src/discord/interact.rs
git commit -m "feat: Mark safe lifts the bot's own timeout"
```

---

## Self-Review

- **Spec coverage:** Req 1 → Task 1; Req 2 → Task 2; Req 3 → Task 4 (needs the `untimeout` sink method from Task 3, so Task 3 precedes Task 4); Req 4 → Task 3.
- **Ordering:** Tasks run sequentially. Tasks 3 and 4 both change the `ActionSink` fakes, so they must not run in parallel. Tasks 1 and 2 touch disjoint files from each other, but all four edit/compile in one crate; keep sequential for clean per-task review.
- **Type consistency:** `timeout(.., reason: &str)`, `untimeout(guild, user, reason)`, `timeout_reason(..) -> String`, `Release`, `lift_bot_timeout(&IncidentRow, u64) -> Release`, `safe_reply(&Release) -> String`, `ocr_eligible(&MemberProfile, &TierConfig, DateTime<Utc>) -> bool` are used with identical names in every task that references them.
- **Out of scope (by design):** config.toml thresholds, kick/ban audit reasons, delete reasons, restoring deleted messages, deploying to the server.
