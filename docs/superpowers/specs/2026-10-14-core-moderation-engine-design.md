# Core Moderation Engine - Design

Status: draft for review. Project 1 of 4 (see "Roadmap").

## 1. Purpose

A Rust Discord moderation bot for the operator's own servers. It detects spam, scams
and abuse using Paxmod (including its Player Context conversation analysis), Jev and
OCR of image text (OCR.space, the engine behind Copyfish), and it watches newly joined and newly created
accounts much more closely than established members, because most spam comes from
those accounts.

Success criteria:

- New and young accounts are scanned by both Paxmod and Jev on every message;
  established members are scanned only on a local-rule hit or a random sample.
- A new guild starts in Observe mode. Observe logs what the bot would have done and
  performs no Discord action.
- Text inside images is extracted and judged like message text, so posting a scam as
  a screenshot does not bypass detection.
- Paxmod receives message text plus `user_id` and `context_id` so Player Context can
  assess whole conversations, not isolated messages.
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
| 3 | Server defenses: raid/lockdown, invite and scam-link intel, image *content* scanning (NSFW/violence via Paxmod `/image`), anti-nuke | later |
| 4 | Dashboard and analytics | later |

OCR of text in images *is* in scope here (section 4.3). Classifying what an image
depicts is not. Out of scope here: everything else in projects 2-4. Core only emits the *signals* later
projects need (strike counts, incident records).

Minimal operator surface that *is* in scope: a `/guard` command group with
`status` and `mode observe|enforce` (requires Manage Guild), plus two mod-log
buttons (Undo, Mark safe). Without these the operator could not switch modes or
reverse a mistake. All other settings live in `config.toml`.

## 3. Decisions

| Decision | Choice |
|---|---|
| Audience | Operator's own server(s); `guild_id` is a column everywhere so multi-guild works, but there is no tenancy, billing or per-guild keys |
| Language / stack | Rust, tokio, serenity 0.12.5 + poise 0.7.1, SQLite via sqlx, reqwest |
| Detection layers | Local rules, Paxmod, Jev, behind a `Detector` trait; OCR feeds text into all of them |
| OCR | OCR.space API (the engine Copyfish uses; Copyfish itself is a manual browser extension and cannot be called by a bot) |
| API budget | Risk-tiered scanning, daily caps, circuit breaker |
| Enforcement | Observe mode by default, then a graduated ladder |
| Secrets | Environment only (`.env` in dev), never logged |

## 4. External services

### 4.1 Paxmod

- `POST https://www.paxmod.com/api/v1/text`, `Authorization: Bearer <key>`.
- Body: `message` (max 5000 chars, bot truncates), `user_id` (Discord user ID),
  `context_id` (Discord channel ID; the thread ID inside threads).
- Returns per-category `score` (0-1) for categories enabled in the Paxmod project.
  The bot reads **scores**, not Paxmod's `flagged`, and applies its own thresholds in
  the fusion step. Advanced-mode projects return one `Policy Violation` score (the operator's
  written policy), which fusion treats as spam risk and shows Paxmod's `reason` as
  evidence. Standard-mode project requirements: type Text, with at
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
- Plan limits: Paxmod's own pages disagree (the OpenAPI spec says 100 messages/month
  free; `llms.txt` says no free plan, a 14-day trial of 10,000 messages, then paid
  plans from 50,000 credits). Treat the operator's console as the source of truth and
  set `PAXMOD_DAILY_CAP` from it.

#### Player Context

Paxmod's public docs describe Player Context as reading whole conversations to find
risk that builds over many messages (for example grooming-style escalation, repeated
solicitation). It is driven by `user_id` and `context_id` on `/text`; the public
OpenAPI lists no separate endpoint (`/api/v1/context` returns 404) and no
Player-Context response fields. The bot therefore:

- Sends `user_id` and `context_id` on **every** Paxmod text call, with the same
  `context_id` for everyone in a channel or thread, so both sides of a conversation
  are visible.
- Has a context feed (`paxmod.context_feed` in `config.toml`):
  - `scanned`: only messages that are scanned anyway are sent.
  - `active_channels` (default): additionally send every human message in a channel
    or thread while a Quarantine or Fresh member has spoken there in the last 10
    minutes, so Paxmod sees the surrounding conversation cheaply.
  - `all`: send every human message. Highest coverage and cost; defeats risk tiering
    for Paxmod.
  Any Paxmod result obtained for a fed message is used like a normal scan result.
- Never caches Paxmod calls when the feed applies (the verdict cache, section 5.3,
  covers Jev and OCR only), because Paxmod needs to see each message and author.
- Does not send join-screening name checks with a `context_id`, so names do not
  pollute conversation history.
- Parses the response tolerantly: known fields are read as in 4.1; unknown top-level
  fields (for example a `player_context` object) are preserved verbatim in the
  incident's `paxmod_json` and shown in the mod-log evidence. Fusion only uses fields
  once their shape is confirmed (open item 6). Until then Player Context improves
  Paxmod's own category scores and nothing else.
- Cannot send Discord message IDs: the API has no such field. The incident row stores
  the returned Paxmod `id` next to the Discord message ID for manual correlation.

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

### 4.3 OCR.space (Copyfish's engine)

- `POST https://api.ocr.space/parse/image` (PRO accounts get regional endpoints; set
  `OCR_BASE_URL`), multipart form, key in an `apikey` header.
- Parameters used: `base64Image` (or `file`), `OCREngine=2` (default; strong on memes,
  noisy and stylized backgrounds, rotated text), `language=auto`,
  `detectOrientation=true`, `scale=true` (helps low-resolution screenshots),
  `isOverlayRequired=false`. Engine 3 (more accurate, slower, 1000 free conversions
  per month) is selectable through `OCR_ENGINE`.
- Result: `ParsedResults[].ParsedText` joined; success when `OCRExitCode` is 1 and
  `IsErroredOnProcessing` is false. Free tier limits: 25,000 requests per month,
  500 per day per IP, 1 MB file size. PRO: 300,000 per month, 5 MB.
- Supported formats: png, jpg, gif, tif, bmp (not webp). Animated GIFs: first frame
  only. Known limitation.

**Image acquisition.** The bot downloads the image itself rather than handing a URL
to OCR.space:

- Sources: message attachments with an image content type, and embed images. Only
  Discord CDN hosts (`cdn.discordapp.com`, `media.discordapp.net`) are fetched; for
  embeds the Discord `proxy_url` is used, never an arbitrary external URL (no SSRF,
  no leaking the bot's IP to attacker servers).
- Hard download cap (default 8 MB) and a cap of 4 images per message.
- Decode, convert webp and others to PNG/JPEG, and downscale or recompress to fit
  the plan's size limit (default 1 MB) using the `image` crate. Images that cannot be
  decoded are skipped and noted in coverage.

**Text handling.** Extracted text is trimmed; fewer than 4 meaningful characters is
treated as "no text". The result is cached by SHA-256 of the image bytes for 1 hour,
so reposted spam images cost one OCR call. OCR text is passed through the same local
rules as message text (links, invites, blocked domains, crypto bait) and appended to
the Paxmod input as `message + "\n[image text] " + ocr_text` (truncated to 5000), and
sent to Jev as a separate `image_text` field in `state`.

Clients (Paxmod, Jev, OCR) are thin `reqwest` wrappers with a base-URL override (for
tests) and a per-request timeout (default 1500 ms for Paxmod and Jev, 4000 ms for
OCR).

## 5. Architecture

Single binary, one tokio runtime.

```
gateway   serenity handlers: message_create, message_update, guild_member_addition,
          guild_member_update, interaction_create
profile   builds MemberProfile
risk      MemberProfile + history -> RiskTier
rules     free local signals (run on message text and OCR text)
ocr       image acquisition, resize, OCR.space client, SHA-256 cache
feed      Paxmod context feed (which messages are sent for Player Context)
detect    trait Detector { async fn assess(&self, &Ctx) -> Result<Assessment> }
            PaxmodDetector, JevDetector
cache     detector cache keyed by normalized-text hash (Jev, OCR; not Paxmod when feed applies)
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
3. Run local rules (section 7.1) on the message text. Record the message ID in
   `recent_messages` (once; edits do not add a second row).
4. Choose the **detection plan** from the tier and the local score (section 6.3).
5. If the message has images and the plan includes OCR, run OCR now (own timeout,
   budget and circuit breaker), then re-run the local rules on the OCR text and merge
   the signals. OCR failure or timeout means "no image text"; the message continues
   text-only and the coverage records it. There is no late retroactive pass in v1.
6. For each planned provider: check budget and circuit breaker, then run Paxmod and
   Jev **concurrently** with a hard timeout on the combined text. A skipped or failed
   provider yields "no result", never a failure of the message. If the context feed
   applies to a message the plan would skip, send it to Paxmod only, as a
   fire-and-forget call.
7. Fuse into a `Verdict` (section 7).
8. `policy` maps (tier, verdict, history) to an action (section 8).
9. In Enforce mode, re-read the guild mode, re-check permissions and hierarchy, then
   execute. In Observe mode record the intended action only.
10. Write the incident row, then post to the mod-log (when action or flag).
11. If the action was delete or stronger on a Quarantine or Fresh member, **sweep**
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
- On overload, shed lowest-tier work first: context-feed-only sends, Established
  sampled scans, Established local-hit scans, then Fresh scans. Quarantine is never
  shed.
- Detector cache: normalized-text hash to Jev results, TTL 10 minutes. Identical
  cross-posted text costs one Jev call. (Paxmod is exempt when the context feed
  applies; OCR has its own image-hash cache, section 4.3.) The cache stores detector
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

> **Revision 2026-10-17 (sleeper-alt hardening):** Established must be *earned by
> history*, not time. After the time-based base tier, a member who would be
> Established but has fewer than `graduation_clean_messages` clean scanned messages
> is downgraded to Fresh (and thus stays fully scanned, Plan::ALL). This neutralizes
> parked cold alts aging past the time gates, and also covers missing `joined_at`
> (which falls back to account age): without history the member is Fresh. The
> downgrade applies before the escalation modifiers below, so a flagged sleeper
> drops Fresh → Quarantine. History DB errors yield zero clean scans, which
> downgrades — the fail-safe direction.

**Modifiers** (applied after the base tier and the downgrade rule, never to Trusted):

- Escalate up one tier (max Quarantine) if the member has an active strike, a prior
  flagged message in the last 30 days, a first message containing a link or mention,
  or a join-screening flag. Join waves are not handled here; Discord's own raid
  protection covers them.
- Clean-message graduation: a Fresh member with at least N clean scanned messages
  (default 10) drops from "scan every message" to "sample" (section 6.3). It does
  not change the tier label or the action thresholds. Any flag resets the count.

### 6.3 Detection plan

| Tier | Local rules | Paxmod + Jev | OCR on images |
|---|---|---|---|
| Trusted | Mass-mention and `@everyone` rules only | Never | Never |
| Quarantine | Always | Every message | Every image |
| Fresh | Always | Every message until graduated; then on local hit (score >= 0.30) or 25% sample | Every image until graduated; then on local hit or 25% sample |
| Established | Always | On local hit (score >= 0.30) or 5% sample | On local hit, or an image-only message with a link, mention or very short text; plus 5% sample |

In addition, the Paxmod context feed (section 4.1) sends messages for Player Context
irrespective of this table.

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
| Image text bait | OCR text contains a URL, invite, wallet address or giveaway wording; or a Quarantine/Fresh member posts an image-only message whose OCR text is a URL or invite |

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
- `coverage`: which sources (local, Paxmod, Jev, OCR) ran, were skipped, were degraded or failed.
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
score, label, action (or "would have" in Observe), coverage, evidence, a
truncated copy of the message (500 chars) and, when present, the extracted image text
(500 chars) with the image thumbnail URL. Buttons:

- **Undo**: removes the timeout or unbans, and logs the reversal. Messages cannot be
  restored. Requires Moderate Members or Ban Members respectively.
- **Mark safe**: adds the member to the allowlist (Trusted) and clears the strike
  from this incident. Requires Manage Guild.

Degradation notices (provider down, over budget, circuit open or closed again) are
posted once per state change, not per message.

## 9. Budget and resilience

- Daily caps per provider (Paxmod, Jev, OCR; defaults from env), counted in
  `api_usage` by UTC day. The OCR default cap is 400 because the free OCR.space tier
  allows 500 requests per day per IP. Context-feed-only sends to Paxmod count against
  the Paxmod cap and stop first when the reserve threshold is reached. A share of the cap (default 50%) is reserved for Quarantine and Fresh: when usage
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
| `incidents` | id, guild_id, user_id, channel_id, message_id, tier, local_signals, paxmod_json (includes Paxmod `id` and any Player Context fields), jev_json, ocr_json (per-image hash, status, excerpt of text, no image bytes), verdict_json, action_planned, action_taken, outcome, text_excerpt, created_at |
| `api_usage` | day, provider, calls, failures |

## 11. Configuration

Secrets and operational values come from the environment (`.env` in development; see
`.env.example`). Behavior comes from `config.toml` (tier boundaries, rule weights,
fusion weights, ladder thresholds and switches, blocklists, trusted roles, log channel
ID). Per-guild sections are supported as `[guild.<id>]` overrides on top of a
`[defaults]` section. `config.toml` is read at startup; changes need a restart. Only
the mode lives in the database, because it changes at runtime. Channel activity for
the context feed (`active_channels`) is in memory only.

Key settings added by OCR and Player Context: `paxmod.context_feed`
(`scanned` | `active_channels` | `all`), `ocr.engine`, `ocr.max_images_per_message`,
`ocr.max_download_bytes`, `ocr.min_text_chars`.

Required intents: Guilds, Guild Members (privileged), Guild Messages, Message Content
(privileged), Guild Moderation. Required permissions: View Channels, Manage Messages,
Read Message History, Moderate Members, Kick Members, Ban Members, and Send Messages
and Embed Links in the log channel.

## 12. Privacy and security

- Message text of scanned messages is sent to Paxmod and Jev. This must be stated in
  the operator's server rules or privacy notice. Discord user and channel IDs are
  sent to Paxmod (and, with the context feed, text of messages from members who would
  not otherwise be scanned); Jev receives profile facts, not IDs.
- Images from scanned messages are sent to OCR.space (a9t9 software GmbH, whose
  privacy policy states it does not store submitted documents; verify before relying
  on it). Image bytes are not stored by the bot.
- Only Discord CDN URLs are fetched (section 4.3).
- Logs contain IDs, tiers, scores and action names, never message content or secrets.
- API keys and the bot token are read from the environment and wrapped in a
  redacting type (`Debug` prints `***`).
- `.env` is gitignored; only `.env.example` is committed.

## 13. Testing

- Unit tests: tier computation (including clock edge cases and modifiers), local
  rules and normalization, fusion math, policy thresholds, budget and circuit
  breaker state machines.
- Detector tests against a `wiremock` server: success, timeout, 429, 503, degraded
  Paxmod, Paxmod ban hit, malformed Jev JSON, schema mismatch. Paxmod requests are
  asserted to carry `user_id` and `context_id`; an unknown `player_context` response
  field is preserved, not a parse error.
- OCR tests: success, empty text, `IsErroredOnProcessing`, timeout, 429, oversized
  image downscaled under the limit, undecodable file, non-Discord host refused, image
  hash cache hit, and a fixture screenshot of a scam link whose OCR text triggers the
  image-text-bait rule.
- Context feed tests: `scanned`, `active_channels` (window opens and expires) and
  `all` send the expected messages; Paxmod is never served from cache when the feed
  applies.
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
   on a plan whose credits cover the scan volume plus the context feed.
2. A TypeSafe API key with Jev access (early access).
3. A Discord application and bot token with the two privileged intents enabled.
4. The log channel ID and trusted role IDs for `config.toml`.
5. A small labeled sample of real spam and legitimate messages for the replay
   harness (the more the better; without it thresholds stay at defaults).
6. Player Context: confirm whether your Paxmod project exposes a separate endpoint or
   extra response fields for it (the public docs show neither). Paste an example
   response and fusion will use it; until then it is stored as evidence only.
7. An OCR.space API key (free tier: 25,000 requests per month, 500 per day, 1 MB per
   image; PRO for more). Confirm that "use Copyfish" meant the OCR.space engine.

Verified live (2026-10-16): the operator's Paxmod project is advanced mode; its `/text`
response carries no Player Context fields (only `user_id`/`context_id` are inputs);
OCR.space Engine 2 reads clean screenshot text accurately.
