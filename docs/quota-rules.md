# Quota Rules

`Quota rules revision: 2026-08-08-01`
`Last verified: 2026-08-08`

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
| Internal identifier | The Rust-side identifier(s) used in code (field/function/const names) |
| Source | Where the data comes from: endpoint(s), credential/auth mechanism |
| Quota scope | Whether the fetched number is a **shared** quota (spans multiple products/tools) or an **independent** quota (specific to this one integration) |
| Short window | What the short-term ("session") window represents for this provider, and its actual duration if fixed |
| Long window | What the long-term ("weekly") window represents for this provider, and its actual duration if fixed |
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
| Internal identifier | `claude_code` (`AppUsageData.claude_code`), `poll_claude_code`, `claude_usage_from_response` |
| Source | `https://api.anthropic.com/api/oauth/usage`, authenticated with the OAuth token from `~/.claude/.credentials.json` (the Claude Code CLI's own login session) |
| Quota scope | Shared — this is the Claude / Claude Code account-level usage window, not a Claude Code-specific metric |
| Short window | `five_hour` bucket from the API response |
| Long window | `seven_day` bucket from the API response |
| Fallback behavior | None beyond the existing credential-source fallback (Windows / WSL) |
| Unavailable conditions | No credentials found; credentials expired (no automatic CLI refresh by default) |
| Minimum fetchable unit | A single aggregate percentage + reset time per window |
| Display caveats | None known — the label "Claude" is expected to match the fetched data without qualification |
| Last verified | 2026-08-08 |
| Rule revision | 2026-08-08-01 |

### ChatGPT

| Field | Value |
|---|---|
| Display name | ChatGPT |
| Internal identifier | `codex` (`AppUsageData.codex`), `poll_codex`, `codex_usage_from_response`, `apply_codex_window` |
| Source | `https://chatgpt.com/backend-api/wham/usage`, authenticated with the Codex CLI's OAuth token, sent with a `ChatGPT-Account-Id` header |
| Quota scope | Shared — the fetch target is literally the ChatGPT backend, reached via the Codex CLI's credentials |
| Short window | Window classified by `limit_window_seconds == 18_000` (5 hours), independent of window position in the API response |
| Long window | Window classified by `limit_window_seconds == 604_800` (7 days), independent of window position in the API response |
| Fallback behavior | None |
| Unavailable conditions | No Codex CLI credentials found |
| Minimum fetchable unit | A single aggregate percentage + reset time per window |
| Display caveats | The label reads "ChatGPT" but the data is fetched through the Codex CLI's login — displayed this way because the endpoint and account are ChatGPT's, not because this is a separate ChatGPT-app-specific integration |
| Last verified | 2026-08-08 |
| Rule revision | 2026-08-08-01 |

### Antigravity

| Field | Value |
|---|---|
| Display name | Antigravity |
| Internal identifier | `antigravity` (`AppUsageData.antigravity`), `poll_antigravity`, `antigravity_usage_from_summary`, gated behind the `antigravity` Cargo feature |
| Source | Google Cloud Code / Antigravity quota endpoints, authenticated with the OAuth token from Windows Credential Manager target `gemini:antigravity` |
| Quota scope | Independent — this is Antigravity's own quota system, not a standalone Gemini API/app quota |
| Short window | `5h`-labeled bucket within whichever quota summary group is selected (see fallback behavior) |
| Long window | `weekly`-labeled bucket within whichever quota summary group is selected (see fallback behavior) |
| Fallback behavior | The quota summary can contain multiple model-family groups (Gemini, Claude, GPT, image models). The fetch prefers the group whose name/description/bucket IDs match "Gemini"; if no Gemini group is present, it falls back to the first group it was able to parse, and further falls back to a separate model-quota endpoint if the summary itself is unavailable |
| Unavailable conditions | No Antigravity credentials found; `antigravity` Cargo feature not compiled in (default release build did not include it until 2026-08-08 — see rule revision 2026-08-08-01 in the app's own change history) |
| Minimum fetchable unit | A single aggregate percentage + reset time per window, taken from one selected group — not a per-model breakdown |
| Display caveats | The number is **not guaranteed to be Gemini's quota specifically**. It is Gemini's quota when a Gemini group is present in the summary, and some other model's quota otherwise. This is why Gemini is not listed as its own separate provider in this document — see [Section 4](#4-conditions-for-adding-a-future-provider) |
| Last verified | 2026-08-08 |
| Rule revision | 2026-08-08-01 |

## 4. Conditions for Adding a Future Provider

A new provider may be added only when all of the following hold:

1. **Independently fetchable.** Its usage data can be retrieved on its own,
   without depending on another provider's fallback chain.
2. **Label/source gap is explainable.** If the display name and the actual
   data source diverge (as with ChatGPT/Codex today), that gap must be
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
| 2026-08-08-01 | 2026-08-08 | Initial version. Documents Claude, ChatGPT, and Antigravity as of the `Claude Code`→`Claude` and `Codex`→`ChatGPT` display-label changes, and the Antigravity release-build feature-flag fix. |
