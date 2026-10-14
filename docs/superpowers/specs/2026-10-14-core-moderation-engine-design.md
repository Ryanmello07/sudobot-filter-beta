# Core Moderation Engine - Design

Status: draft for review. Project 1 of 4 (see "Roadmap").

## 1. Purpose

A Rust Discord moderation bot for the operator's own servers. It detects spam, scams
and abuse using Paxmod and Jev, and it watches newly joined and newly created
accounts much more closely than established members, because most spam comes from
those accounts.

Success criteria:

- New and young accounts are scanned by both Paxmod and Jev on every message;
  established members are scanned only on a local-rule hit or a random sample.
- A new guild starts in Observe mode. Observe logs what the bot would have done and
  performs no Discord action.
- A spam account that trips the delete threshold has its recent messages across all
  channels removed, not just the triggering message.
- If Paxmod or Jev is down, slow or over budget, the bot keeps moderating with local
  rules and says so in the mod-log. It never blocks message handling on a provider.
- Thresholds can be tuned against a labeled message file (replay harness) before
  Enforce mode is switched on.

## 2. Roadmap and scope

| # | Project | Status |
|---|---|---|
| 1 | Core engine: pipeline, risk tiers, actions, mod-log | this spec |
| 2 | Moderator tooling: commands, cases, strikes UI, review queue, appeals | later |
| 3 | Server defenses: raid/lockdown, invite and scam-link intel, image and attachment scanning, anti-nuke | later |
| 4 | Dashboard and analytics | later |

Out of scope here: everything in projects 2-4. Core only emits the *signals* later
projects need (join-wave signal, strike counts, incident records).

Minimal operator surface that *is* in scope: a `/guard` command group with
`status` and `mode observe|enforce` (requires Manage Guild), plus two mod-log
buttons (Undo, Mark safe). Without these the operator could not switch modes or
reverse a mistake. All other settings live in `config.toml`.

## 3. Decisions

| Decision | Choice |
|---|---|
| Audience | Operator's own server(s); `guild_id` is a column everywhere so multi-guild works, but there is no tenancy, billing or per-guild keys |
| Language / stack | Rust, tokio, serenity 0.12.5 + poise 0.7.1, SQLite via sqlx, reqwest |
| Detection layers | Local rules, Paxmod, Jev, behind a `Detector` trait |
| API budget | Risk-tiered scanning, daily caps, circuit breaker |
| Enforcement | Observe mode by default, then a graduated ladder |
| Secrets | Environment only (`.env` in dev), never logged |

## 4. External services

### 4.1 Paxmod

- `POST https://www.paxmod.com/api/v1/text`, `Authorization: Bearer <key>`.
- Body: `message` (max 5000 chars, bot truncates), `user_id` (Discord user ID),
  `context_id` (Discord channel ID).
- Returns per-category `score` (0-1) for categories enabled in the Paxmod project.
  The bot reads **scores**, not Paxmod's `flagged`, and applies its own thresholds in
  the fusion step. Paxmod project requirements: type Text, standard mode, with at
  least Spam, Harassment, Hate and Discrimination, Threatening, Personally
  Identifiable Information and Dangerous and Criminal Content enabled.
- Special results:
  - `degraded: true`: reduced coverage. Treated as a partial result and noted in the
    incident.
  - `flagged: true` with empty `categories`: user is on the Paxmod console ban list
    or the text was allow-listed. If `reason` indicates a ban, this is a strong
    signal (see 7.2). Otherwise it is ignored.
  - HTTP 429 `USAGE_LIMIT_EXCEEDED`: not retried; trips the Paxmod budget for the
    rest of the day.
  - HTTP 503 `LLM_SERVICE_ERROR`, 5xx, timeouts: counted by the circuit breaker.
- Free plan is 100 messages/month, so a paid plan is required in practice.

### 4.2 Jev (TypeSafe System One)

- `POST https://api.typesafe.ai/v1/systemone`, `Authorization: Bearer <key>`.
- Body: `state` (string or object), `model` (default `jev-latest`, overridable by
  env), `questions` (map of id to typed question).
- One call carries all questions and returns one answer each: `choice` with
  `probabilities` and `confidence`, `noul` as a 0-1 probability.
- Questions (v1):

| id | type | definition |
|---|---|---|
| `class` | choice | `legitimate`, `spam`, `scam_phishing`, `promo_solicitation`, `nsfw_lure`, `other_abuse` |
| `dm_lure` | noul | The message pushes people to DM the author or to join another server or channel |
| `crypto_giveaway` | noul | The message promotes a giveaway, airdrop, investment or similar bait |
| `evasion` | noul | The text is written to dodge a filter (spacing, homoglyphs, split links) |
| `bot_like` | noul | The author's recent messages and profile look automated or templated |

- `state` is a structured object: current message text (truncated, default 2000
  chars), up to 3 prior messages from the same author in the last 10 minutes (each
  truncated to 500 chars, in-memory only), and profile facts (account age in days,
  minutes in guild, has default avatar, role count). The state schema and question
  instructions live in one file so they can be tuned together.
- Jev cannot explain itself. Evidence shown to moderators is the answers and
  probabilities, not a rationale.
- Early access product: the exact answer JSON is validated at the boundary. A schema
  mismatch counts as a provider failure for the circuit breaker and is logged.

Both clients are thin `reqwest` wrappers with a base-URL override (for tests) and a
per-request timeout (default 1500 ms).

## 5. Architecture

Single binary, one tokio runtime.

```
gateway   serenity handlers: message_create, message_update, guild_member_addition,
          guild_member_update, interaction_create
profile   builds MemberProfile
risk      MemberProfile + history -> RiskTier
rules     free local signals
detect    trait Detector { async fn assess(&self, &Ctx) -> Result<Assessment> }
            PaxmodDetector, JevDetector
cache     verdict cache keyed by normalized-text hash
budget    daily caps, reserved share, circuit breaker per provider
pipeline  one message, end to end
fusion    local + Paxmod + Jev -> Verdict
policy    (tier, Verdict, history) -> Action
actions   trait ActionSink { delete, bulk_delete, timeout, kick, ban }, Discord impl
modlog    embeds and buttons
store     SQLite access
config    config.toml + env
```

`Detector` and `ActionSink` are traits so the pipeline can be tested with fakes.

### 5.1 Per-message flow

1. Ignore: bots, DMs, system messages, webhook messages, empty content.
2. Build `MemberProfile` and compute the tier (section 6).
3. Run local rules (section 7.1). Record the message ID in `recent_messages` (once;
   edits do not add a second row).
4. Choose the **detection plan** from the tier and the local score (section 6.3).
5. For each planned provider: check budget and circuit breaker, then run Paxmod and
   Jev **concurrently** with a hard timeout. A skipped or failed provider yields
   "no result", never a failure of the message.
6. Fuse into a `Verdict` (section 7).
7. `policy` maps (tier, verdict, history) to an action (section 8).
8. In Enforce mode, re-read the guild mode, re-check permissions and hierarchy, then
   execute. In Observe mode record the intended action only.
9. Write the incident row, then post to the mod-log (when action or flag).
10. If the action was delete or stronger on a Quarantine or Fresh member, **sweep**
    that member's messages from the last 10 minutes (`recent_messages`).

`message_update` follows the same flow, but only when the event contains content and
author; partial updates are skipped. Spammers edit links in after posting.

### 5.2 Join-time screening

On `guild_member_addition`, run local profile checks (username/display name patterns,
default avatar plus very young account) and Paxmod and Jev on the name fields. Join
screening **only escalates the member's tier and posts a mod-log alert**; it never
kicks or bans, because names alone are weak evidence.

### 5.3 Concurrency and backpressure

- A bounded queue (default 256) feeds a fixed worker pool (default 8).
- Messages are sharded to workers by `hash(guild_id, user_id)`, so one author's
  messages are processed in order.
- On overload, shed lowest-tier work first: Established sampled scans, then
  Established local-hit scans, then Fresh scans. Quarantine is never shed.
- Verdict cache: normalized-text hash to detector results, TTL 10 minutes. Identical
  cross-posted text costs one API call per provider. The cache stores detector
  outputs, not actions, so each author's tier still drives the decision.

## 6. New-account vigilance

### 6.1 Clocks

- Account age: from the snowflake timestamp in the user ID. Free and unforgeable.
- Time in guild: from `joined_at`.

### 6.2 Tiers (defaults; all configurable)

| Tier | Rule |
|---|---|
| Trusted | Has a configured trusted role, has Manage Messages or higher, is the guild owner, or is on the allowlist |
| Quarantine | Not Trusted, and (in guild < 30 minutes OR account < 24 hours old) |
| Fresh | Not Trusted or Quarantine, and (in guild < 7 days OR account < 30 days old) |
| Established | Everyone else |

Evaluation order: Trusted, Quarantine, Fresh, Established. Note the OR: an old account
that just joined is Quarantine, and a brand-new account that has been in the guild
for a day is Fresh until it passes both thresholds.

**Modifiers** (applied after the base tier, never to Trusted):

- Escalate up one tier (max Quarantine) if the member has an active strike, a prior
  flagged message in the last 30 days, a first message containing a link or mention,
  a join-screening flag, or is part of a join wave (3 or more joins within 2 minutes
  with account ages within 1 hour of each other; this is only a signal, lockdown is
  project 3).
- Clean-message graduation: a Fresh member with at least N clean scanned messages
  (default 10) drops from "scan every message" to "sample" (section 6.3). It does
  not change the tier label or the action thresholds. Any flag resets the count.

### 6.3 Detection plan

| Tier | Local rules | Paxmod + Jev |
|---|---|---|
| Trusted | Mass-mention and `@everyone` rules only | Never |
| Quarantine | Always | Every message |
| Fresh | Always | Every message until graduated; then on local hit (score >= 0.30) or 25% sample |
| Established | Always | On local hit (score >= 0.30) or 5% sample |

### 6.4 Threshold shifting

The same fused score leads to a more severe action for newer tiers (section 8.1). Trust
is earned by behavior, so a patient account that posts harmlessly and then turns
spammy is re-escalated by its own strikes and flags.

## 7. Detection and fusion

### 7.1 Local rules (free, every message)

| Rule | Signal |
|---|---|
| Flood | N messages in T seconds from one author (default 5 in 8 s) |
| Duplicate | Same normalized text across 2 or more channels, or 3 or more times, in 2 minutes |
| Mass mention | More than 5 distinct user or role mentions |
| Everyone attempt | `@everyone` or `@here` text from a member without permission to use it |
| First-message link | Author's first message in this guild contains a URL |
| Invite link | Discord invite to a guild not on the allowlist |
| Blocked domain | Domain on the configured blocklist, or a known URL shortener |

Text normalization: NFKC, strip zero-width characters, lowercase, collapse whitespace,
map common homoglyphs. Used for duplicate detection and cache keys. The original text
is what goes to Paxmod and Jev (Paxmod also normalizes server-side).

Each rule has a weight in 0-1. Local score is the noisy-OR of fired rules:
`1 - product(1 - w_i)`.

### 7.2 Fusion

Each source yields a risk in 0-1 plus a "present" flag:

- Local: the local score.
- Local: the local score (counts toward spam risk only).
- Paxmod: spam risk = `max(Spam, Dangerous and Criminal Content)`; abuse risk =
  `max(Harassment, Hate and Discrimination, Threatening, Personally Identifiable
  Information)`. A Paxmod console-ban hit sets spam risk to 1.0.
- Jev: spam risk = the greatest of `1 - P(class = legitimate)`,
  `0.9 * max(dm_lure, crypto_giveaway)` and `0.7 * evasion`. Abuse risk =
  `P(class = other_abuse)`. `confidence` and the other answers are kept as
  evidence only.

Spam risk and abuse risk are fused **separately** with a weighted noisy-OR over the
present sources: `1 - product(1 - w_s * r_s)`, default weights local 0.6, Paxmod 0.9,
Jev 0.9. The verdict's `score` is the larger of the two fused values. The verdict
also carries:

- `label`: `abuse` if the abuse fused value is the larger; otherwise `scam` if Jev's
  `class` choice is `scam_phishing` (or `nsfw_lure`), else `spam`; `clean` if the score
  is below the lowest flag threshold.
- `agreeing_sources`: number of sources with risk >= 0.5.
- `coverage`: which sources ran, were skipped, were degraded or failed.
- `evidence`: rule names, the top Paxmod categories with scores, Jev answers with
  probabilities.

Weights and thresholds are config values and are tuned with the replay harness.

## 8. Policy and actions

### 8.1 Action ladder (fused score thresholds, defaults)

| Action | Quarantine | Fresh | Established |
|---|---|---|---|
| Flag in mod-log | 0.40 | 0.50 | 0.65 |
| Delete message | 0.60 | 0.70 | 0.85 |
| Delete + timeout (default 60 min) | 0.70 | 0.80 | 0.92 |
| Kick | 0.85 | 0.92 | off |
| Ban (and delete 1 h of history) | 0.95 | 0.97 | off |

Extra rules:

- Kick and ban require `agreeing_sources >= 2`.
- Abuse (non-spam) labels cap at Delete + timeout. Only spam and scam labels can
  reach kick or ban.
- Each rung has an enable switch. Disabled rungs fall back to the next lower enabled
  one.
- Trusted members are never actioned. Joins are never actioned (5.2).
- The `agreeing_sources >= 2` requirement for kick and ban is waived for a member
  with an active strike only when `repeat_offender_single_source` is true
  (default false).

### 8.2 Execution safety

- Mode is Observe or Enforce, stored per guild; a guild with no `guild_state` row is
  Observe. In Observe nothing is deleted, timed out, kicked, banned or swept; the
  incident records the planned action and the mod-log shows "would have". It is re-read
  immediately before any action, so switching to Observe cancels in-flight actions.
- Before acting, check bot permissions and role hierarchy against the target. The bot
  never acts on the owner, on members above it in the hierarchy, or on Trusted
  members. A refused action is recorded as a failure, not retried.
- Actions are idempotent per message ID.
- Strikes: a timeout, kick or ban adds a strike row (used for tier escalation).
  Strikes expire after 30 days by default.
- Sweep: bulk-delete messages from `recent_messages` for that author within 10
  minutes. Discord limits bulk delete to messages under 14 days and 2-100 per call;
  single leftovers are deleted individually.

### 8.3 Mod-log

One embed per flagged or actioned incident: tier, account age, time in guild, fused
score, label, action (or "would have" in Observe), coverage, evidence, and a
truncated copy of the message (500 chars). Buttons:

- **Undo**: removes the timeout or unbans, and logs the reversal. Messages cannot be
  restored. Requires Moderate Members or Ban Members respectively.
- **Mark safe**: adds the member to the allowlist (Trusted) and clears the strike
  from this incident. Requires Manage Guild.

Degradation notices (provider down, over budget, circuit open or closed again) are
posted once per state change, not per message.

## 9. Budget and resilience

- Daily caps per provider (defaults from env), counted in `api_usage` by UTC day.
  A share of the cap (default 50%) is reserved for Quarantine and Fresh: when usage
  reaches `cap * (1 - reserve)`, scans of Established members and of graduated Fresh
  members stop first; Quarantine and ungraduated Fresh keep running until the cap.
- Circuit breaker per provider: opens after 5 consecutive failures or timeouts, stays
  open 60 s, then lets a single probe through (half-open). Opening and closing are
  announced in the mod-log.
- Retry: one retry with 200 ms backoff on 5xx or timeout, only when the remaining time
  budget allows; never on 4xx.
- Everything fails open for the provider layer: no provider result means local rules
  decide, and the verdict `coverage` says what was missing.

## 10. Storage

SQLite, migrations in `migrations/`, accessed with `sqlx`. Message text is stored only
in `incidents`, truncated to 500 chars, and pruned after `retention_days`
(default 14, allowed 7-30).

| Table | Key columns |
|---|---|
| `guild_state` | guild_id, mode, updated_by, updated_at |
| `members` | guild_id, user_id, joined_at, clean_scans, last_flag_at, allowlisted |
| `strikes` | id, guild_id, user_id, kind, created_at, expires_at, incident_id |
| `recent_messages` | guild_id, user_id, channel_id, message_id, created_at (IDs only, pruned after 1 h) |
| `incidents` | id, guild_id, user_id, channel_id, message_id, tier, local_signals, paxmod_json, jev_json, verdict_json, action_planned, action_taken, outcome, text_excerpt, created_at |
| `api_usage` | day, provider, calls, failures |

## 11. Configuration

Secrets and operational values come from the environment (`.env` in development; see
`.env.example`). Behavior comes from `config.toml` (tier boundaries, rule weights,
fusion weights, ladder thresholds and switches, blocklists, trusted roles, log channel
ID). Per-guild sections are supported as `[guild.<id>]` overrides on top of a
`[defaults]` section. `config.toml` is read at startup; changes need a restart. Only
the mode lives in the database, because it changes at runtime.

Required intents: Guilds, Guild Members (privileged), Guild Messages, Message Content
(privileged), Guild Moderation. Required permissions: View Channels, Manage Messages,
Read Message History, Moderate Members, Kick Members, Ban Members, and Send Messages
and Embed Links in the log channel.

## 12. Privacy and security

- Message text of scanned messages is sent to Paxmod and Jev. This must be stated in
  the operator's server rules or privacy notice. Discord user and channel IDs are
  sent to Paxmod; Jev receives profile facts, not IDs.
- Logs contain IDs, tiers, scores and action names, never message content or secrets.
- API keys and the bot token are read from the environment and wrapped in a
  redacting type (`Debug` prints `***`).
- `.env` is gitignored; only `.env.example` is committed.

## 13. Testing

- Unit tests: tier computation (including clock edge cases and modifiers), local
  rules and normalization, fusion math, policy thresholds, budget and circuit
  breaker state machines.
- Detector tests against a `wiremock` server: success, timeout, 429, 503, degraded
  Paxmod, Paxmod ban hit, malformed Jev JSON, schema mismatch.
- Pipeline tests with fake `Detector` and `ActionSink`: Observe vs Enforce, mode
  flipped mid-flight, hierarchy refusal, sweep, verdict cache hit, shed under load,
  fail-open when both providers are down.
- Live smoke tests for each real API, `#[ignore]`, run manually with real keys,
  never in CI.
- Replay harness: a CLI that feeds a labeled JSONL file (message, author age, time in
  guild, label) through the pipeline in Observe mode with the real or mocked
  providers and prints precision, recall and the action distribution per tier. Used
  to tune thresholds before enabling Enforce.

## 14. Open items for the operator

1. A Paxmod text project (standard mode, categories listed in 4.1) and its API key,
   on a paid plan if the always-scan tiers will exceed 100 messages/month.
2. A TypeSafe API key with Jev access (early access).
3. A Discord application and bot token with the two privileged intents enabled.
4. The log channel ID and trusted role IDs for `config.toml`.
5. A small labeled sample of real spam and legitimate messages for the replay
   harness (the more the better; without it thresholds stay at defaults).
