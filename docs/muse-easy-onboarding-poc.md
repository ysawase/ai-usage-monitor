# Muse (muse.ai) — easy onboarding / integration contract (experimental)

Status: **PoC ONLY; not shipped; not approved for public distribution**.
This applies to the personal Muse web/PWA agent, **not Muse Code**. The current 1.4.12 Store build must remain untouched.

## User journey (target UX)

1. In the AI Usage Monitor, choose **Add service → Muse**.
2. See one explanatory line: "Sign in to Muse in your browser. We only display your weekly quota; we do not read your chats."
3. Select **Open Muse**. Muse opens at https://muse.ai/ in the user's normal browser. No password, token, Cookie header, extension developer mode, or command prompt is requested.
4. Select **Connect**. A trusted, browser-store-installed companion extension (if available) displays its own clear permission request for muse.ai and for connecting to this app. The monitor must not claim the extension can be auto-installed.
5. The extension queries normalized usage in the signed-in muse.ai page context without extracting cookies, then shares **only** the fields in the observation contract with the local monitor. The app shows **Connected**, the plan, weekly used %, and reset date/time if supplied.
6. When unavailable, show a specific nontechnical action: **Open Muse and sign in**, **Open Muse to refresh**, **Reconnect companion**, or **Try again**. Never show 0% for unavailable/stale data.
7. **Disconnect** revokes the local pairing, stops the bridge, and removes normalized cached usage.

## Integration constraints

- A browser extension using Muse's **undocumented** Next.js `fetchSubscriptionAction` is a *technical* option demonstrated by RateBucket; it is **not** an officially documented API or permission for a distributed product. Meta feedback ticket is submitted; permissions remain unresolved.
- Bridge must be **opt-in**, with localhost-only/native messaging transport, a per-install pairing/authorization boundary, strict message origin and schema validation, payload size bound, and no HTTP listener exposed to the LAN.
- Preferred trust boundary for Windows is documented browser **Native Messaging**: extension ID is allowlisted in the host manifest and registered only during explicit connection setup. Windows/MSIX install registration and Opera/Chrome/Edge compatibility remain **unverified**. If a compliant, store-distributed extension is unavailable, the UI must disclose that connection is not yet supported; do not silently require developer mode or pasted cookies.
- Never read/export/store Muse cookies, OAuth tokens, Authorization headers, conversation text, raw Muse responses, or dynamic web-app script content. Store only normalized usage observations and explicit user preferences.
- Frontend link state is **not_connected/current/stale/invalid**, separate from the quota percentage itself.
- Current observation schema in `src/muse_bridge.rs`: `schema_version=1`, `source=muse_browser_bridge`, `observed_at_unix`, `plan`, `weekly_used_percent`, optional `weekly_resets_at_unix`. Unknown fields, incorrect schema, invalid percent, implausible future timestamp, and oversized payloads are rejected.
- If no new reading for 5 minutes, show **Needs refresh** and hide the current percentage; 5 minutes is an app-level freshness limit, **not a Meta guarantee**. No reset time is invented from date-only DOM data.
- The extension's background lifecycle is constrained by the browser: hidden/closed tabs and asleep browsers cannot be promised to update. A login prompt must never be presented as a silent background authentication attempt.
- `POST https://muse.ai/` via private Next.js action is NOT called by this PoC. This branch currently contains no browser extension or working native-messaging host.
- Full product integration remains separate work: UI/menu/localization (11 languages), normalized cache lifecycle, polling boundary, secure host registration and removal, signed companion distribution, live Windows/Opera/Chrome tests, packaging and Store review/permission gate.

## Technical precedents

- RateBucket: https://github.com/Chihiro521/RateBucket-rate-limit-bucket/blob/master/src/injected/museSubscription.ts — same-origin browser bridge, no cookie extraction, private Next.js action. Browser tab required for updates.
- CodexBar: https://github.com/steipete/CodexBar/blob/main/docs/museai.md — browser/manual cookies; internal server-action discovery.
- Vibe Bar: https://github.com/AstroQore/vibe-bar/blob/main/Sources/VibeBarCore/Adapters/MuseAgentQuotaAdapter.swift — macOS desktop adapter with stored/imported cookies.
- Meta terms: https://muse.ai/terms — legal/terms check required before distribution.

## Acceptance criteria / gates

**Contract PoC:** tests pass for 0%, missing reset, stale, expired, invalid/malformed input and rejected secret-bearing fields.

**Personal live E2E:** the user installs a verified, trusted extension via normal browser-store flow, signs into Muse, clicks Connect, sees a value matching Settings; closes/reopens the tab and browser, disconnects, and confirms correct freshness/failure handling. No credential extraction.

**Store rollout gate (separate):** verify third-party use permission, browser-store availability, Native Messaging registration in Store MSIX deployment, signed packaging, clean install/uninstall, and no user-side developer mode. Explicit user decision required to change the project provider adoption state.

No Store submission, public release, or automated installation is authorized by this PoC.
