# Quota Rules

`Quota rules revision: 2026-08-10-01`
`Last verified: 2026-08-10`

This is the specification of record for how this app interprets and labels
provider usage data. It is not user-facing copy — README stays short and
simple; this document is where the underlying meaning of each provider's
number is defined and kept up to date.

## 1. Common Policy

- This app displays whatever usage quota it can actually fetch, in parallel,
  per provider.
- The on-screen UI stays simple. Any nuance about what a number really means
  belongs here, not in the widget or in README.
- A provider's UI label and its underlying data source are not guaranteed to
  be a 1:1 match. Where they diverge, this document is authoritative — if the
  UI label and this document disagree, this document describes the intended
  and correct meaning; the UI label is the simplified surface of it.
- A provider is added only on the condition that it does not produce a
  misleading display (see [Section 4](#4-conditions-for-adding-a-future-provider)).

## 2. Per-Provider Record Schema

Every provider entry in [Section 3](#3-current-providers) must record the
following fields. When a new provider is added, copy this schema and fill it
in — do not invent a different shape per provider.

| Field | Meaning |
|---|---|
| Display name | The label shown in the widget UI and tray icon |
| Stable family ID | The durable quota-family identifier used in runtime data and snapshots |
| Internal adapter | The Rust-side provider-specific fetch/parse functions |
| Source | Where the data comes from: endpoint(s), credential/auth mechanism |
| Quota scope | Whether the fetched number is a **shared** quota (spans multiple products/tools) or an **independent** quota (specific to this one integration) |
| Quota items | The ordered items supplied by this family, including metric form, unit, and reset meaning |
| Fallback behavior | Whether the fetch path has a fallback when the preferred data isn't available, and what it falls back to |
| Unavailable conditions | When this provider's window(s) should be treated as not available (no data, not applicable, etc.) |
| Minimum fetchable unit | The smallest unit of data the source actually returns (e.g. a single aggregate percentage vs. per-model breakdown) |
| Display caveats | Anything a reader should know before trusting the on-screen number at face value |
| Last verified | Date this entry was last checked against the live implementation |
| Rule revision | The `Quota rules revision` value this entry was last updated under |

## 3. Current Providers

### Claude

| Field | Value |
|---|---|
| Display name | Claude |
| Stable family ID | `claude` |
| Internal adapter | `poll_claude_code`, `claude_usage_from_response` |
| Source | `https://api.anthropic.com/api/oauth/usage`, authenticated with the OAuth token from `~/.claude/.credentials.json` (the Claude Code CLI's own login session) |
| Quota scope | Shared — this is the Claude / Claude Code account-level usage window, not a Claude Code-specific metric |
| Quota items | `session`: percentage + reset from the `five_hour` bucket; `weekly`: percentage + reset from the `seven_day` bucket |
| Fallback behavior | None beyond the existing credential-source fallback (Windows / WSL) |
| Unavailable conditions | No credentials found; credentials expired (no automatic CLI refresh by default) |
| Minimum fetchable unit | A single aggregate percentage + reset time per window |
| Display caveats | None known — the label "Claude" is expected to match the fetched data without qualification |
| Last verified | 2026-08-08 |
| Rule revision | 2026-08-08-01 |

### Codex

| Field | Value |
|---|---|
| Display name | Codex |
| Stable family ID | `codex` |
| Internal adapter | `poll_codex`, `codex_usage_from_response`, `apply_codex_window` |
| Source | 5h/7d usage: `https://chatgpt.com/backend-api/wham/usage`, authenticated with the Codex CLI's OAuth token and a `ChatGPT-Account-Id` header. Banked Full reset count: the stable Codex app-server method `account/rateLimits/read` |
| Quota scope | Shared — the fetch target is literally the ChatGPT backend, reached via the Codex CLI's credentials |
| Quota items | `session`: percentage + reset for `limit_window_seconds == 18_000`; `weekly`: percentage + reset for `limit_window_seconds == 604_800`. Classification is independent of response position |
| Fallback behavior | None for 5h/7d usage. Banked reset retrieval is isolated: if the CLI, app-server lifecycle, timeout, protocol, or response shape fails, only the Full reset count becomes unavailable and the existing usage windows remain valid |
| Unavailable conditions | No Codex CLI credentials found for usage. The Full reset count is independently unavailable when `rateLimitResetCredits` or `availableCount` cannot be obtained; unavailable is not converted to zero |
| Minimum fetchable unit | A single aggregate percentage + reset time per usage window, plus `rateLimitResetCredits.availableCount` for banked resets |
| Display caveats | The family is labeled `Codex` so it is not mistaken for a separately implemented ChatGPT provider. The underlying usage endpoint remains ChatGPT's backend and uses the Codex CLI login |
| Last verified | 2026-08-10 |
| Rule revision | 2026-08-10-01 |

Banked reset rules:

- The count authority is `rateLimitResetCredits.availableCount`; the number of
  optional credit detail rows is never used as the count.
- The monitor checks banked resets only from the existing provider refresh and
  caches the result for five minutes. When that cache expires, it starts
  `codex app-server`, sends `initialize`, `initialized`, and
  `account/rateLimits/read`, then closes the subprocess. It does not add a
  separate high-frequency timer.
- Private backend endpoints are not called, and reset consume/redeem operations
  are never invoked.
- Raw app-server responses, credit IDs, auth tokens, cookies, and credentials
  are not logged or displayed. Credit detail rows and expiry metadata are not
  retained by the UI data model.
- If app-server retrieval fails or the reset-credit field is absent or
  malformed, Full reset is unavailable while the already-fetched 5h/7d usage
  remains unchanged.
- Final live-protocol verification on 2026-08-09 confirmed the stable method
  and `availableCount` shape; the implementation relies only on that minimum
  shape.

### Antigravity

| Field | Value |
|---|---|
| Display name | Antigravity |
| Stable family ID | `antigravity` |
| Internal adapter | `poll_antigravity`, `antigravity_usage_from_summary`, gated behind the `antigravity` Cargo feature |
| Source | Google Cloud Code / Antigravity quota endpoints, authenticated with the OAuth token from Windows Credential Manager target `gemini:antigravity` |
| Quota scope | Independent — this is Antigravity's own quota system, not a standalone Gemini API/app quota |
| Quota items | `session`: percentage + reset from the selected group's `5h` bucket; `weekly`: percentage + reset from its `weekly` bucket |
| Fallback behavior | The quota summary can contain multiple model-family groups (Gemini, Claude, GPT, image models). The fetch prefers the group whose name/description/bucket IDs match "Gemini"; if no Gemini group is present, it falls back to the first group it was able to parse, and further falls back to a separate model-quota endpoint if the summary itself is unavailable |
| Unavailable conditions | No Antigravity credentials found; `antigravity` Cargo feature not compiled in (default release build did not include it until 2026-08-08 — see rule revision 2026-08-08-01 in the app's own change history) |
| Minimum fetchable unit | A single aggregate percentage + reset time per window, taken from one selected group — not a per-model breakdown |
| Display caveats | The number is **not guaranteed to be Gemini's quota specifically**. It is Gemini's quota when a Gemini group is present in the summary, and some other model's quota otherwise. This is why Gemini is not listed as its own separate provider in this document — see [Section 4](#4-conditions-for-adding-a-future-provider) |
| Last verified | 2026-08-08 |
| Rule revision | 2026-08-08-01 |

### GitHub Copilot

| Field | Value |
|---|---|
| Display name | GitHub Copilot |
| Stable family ID | `github_copilot` |
| Internal adapter | `poll_github_copilot`, `github_copilot_usage_from_response` |
| Source | Official GitHub user billing AI-credit usage REST API, `/users/{username}/settings/billing/ai_credit/usage`, invoked through `gh api` |
| Quota scope | Independent monthly Copilot AI Credits for a paid individual account |
| Quota items | `monthly_ai_credits`: summed Copilot `grossQuantity` as used AI Credits; optionally paired with the manually selected plan allowance; reset at the first day of the next calendar month at 00:00 UTC |
| Fallback behavior | None. GitHub CLI is the credential broker; the app does not read, refresh, or store a GitHub token itself |
| Unavailable conditions | `gh` missing or not logged in, insufficient API permission, command/API failure, malformed values, or a non-empty response with no recognizable Copilot AI-credit rows. These conditions are never displayed as zero usage |
| Minimum fetchable unit | Aggregate Copilot AI-credit usage rows. The app sums `grossQuantity`; it deliberately does not use `netQuantity`, which may be zero after included-credit discounts |
| Display caveats | Initial scope is Paid Individual only. Free, Student, Business, and Enterprise are unsupported. Plan is a manual setting: `Pro` = 1,500, `Pro+` = 7,000, `Max` = 20,000 AI Credits as of 2026-08-10. These totals include a flex allotment and may change in GitHub's product specification. `Unknown` shows gross usage only and does not invent limit, remaining, or percentage. Subscription billing date is not treated as quota reset date |
| Last verified | 2026-08-10 |
| Rule revision | 2026-08-10-01 |

Security and retention rules:

- Raw API responses, GitHub tokens, credentials, opaque authentication data,
  per-model billing detail, and individual usage rows are not logged,
  displayed, or retained in settings/snapshots.
- Only the aggregate quota item needed by the UI is retained.
- The app never runs `gh auth refresh`, changes scopes, or mutates the user's
  GitHub authentication state. Authentication failures are surfaced as an
  unavailable/not-configured state.

## 4. Conditions for Adding a Future Provider

A new provider may be added only when all of the following hold:

1. **Independently fetchable.** Its usage data can be retrieved on its own,
   without depending on another provider's fallback chain.
2. **Label/source gap is explainable.** If the display name and the actual
   data source diverge (as with Codex and the ChatGPT backend today), that gap must be
   written down in this document using the schema in
   [Section 2](#2-per-provider-record-schema) before the label ships.
3. **No double display.** The new provider must not show a number that
   already appears (in full or in part) under an existing provider's column.
   Example: Gemini is not added as its own provider today because its data
   already surfaces, conditionally, under Antigravity.
4. **Has a documentation home.** README, and any future Help/About surface,
   must be able to carry a short pointer to this document for the new
   provider — this document is not a substitute for that pointer, and that
   pointer is not a substitute for filling in this document.
5. **Unavailable state is defined.** The conditions under which the new
   provider's window(s) should be treated as unavailable (rather than shown
   with stale or wrong data) must be written into its
   [Section 2](#2-per-provider-record-schema) entry.

If any of these cannot be satisfied yet, the correct action is to document
the provider's current limitation here (or defer it) rather than add a UI
column that misrepresents what is actually being measured.

## 5. Revision Log

| Revision | Date | Change |
|---|---|---|
| 2026-08-10-01 | 2026-08-10 | Generalizes the runtime record to stable quota families with ordered quota items, restores the Codex display name, and adds GitHub Copilot Paid Individual monthly AI Credits with GitHub CLI authentication, gross-usage, manual-plan, reset, failure, and non-retention rules. |
| 2026-08-09-01 | 2026-08-09 | Adds the ChatGPT/Codex banked Full reset count from the stable app-server method, including zero-vs-unavailable semantics, lifecycle, failure isolation, refresh, and non-retention rules. |
| 2026-08-08-01 | 2026-08-08 | Initial version. Documents Claude, ChatGPT, and Antigravity as of the `Claude Code`→`Claude` and `Codex`→`ChatGPT` display-label changes, and the Antigravity release-build feature-flag fix. |
