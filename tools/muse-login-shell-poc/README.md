# Muse Windows WebView2 login shell — isolated PoC

**PERSONAL-LOCAL QUOTA EXPERIMENT. NOT PART OF THE PRODUCTION MONITOR. NOT FOR STORE DISTRIBUTION.**

This minimal .NET 8 WinForms/WebView2 application opens the first-party personal Muse website at `https://muse.ai/` in an app-specific profile. It does not use browser extensions, read normal browser cookies, execute script inside Muse, or send data to the production monitor. In an **explicitly enabled optional self-only mode**, it temporarily uses this dedicated WebView2 profile's own cookies in memory to make first-party Muse subscription requests.

- Dedicated local profile: `%LOCALAPPDATA%\\Solidaily\\MuseLoginShellPoc\\WebView2Profile` (not Chrome/Opera/Edge profiles)
- Navigate only to `https://muse.ai` and `https://auth.muse.ai`, plus `about:blank` for local reset
- Block popups, WebMessage, DevTools, host objects, and browser permission prompts
- An explicit confirmation clears browsing data **only** in this experimental profile; the code never deletes the regular browser profile or user's Muse account
- No credentials or raw webpage response are logged, copied, or stored by custom code. The WebView2 runtime itself persists normal browser session state in the isolated profile.

The demo may be unable to finish authentication if Muse uses additional login domains, popups or provider-specific flows. Do not relax the allowlist without reviewing actual first-party login requirements and consent. No JavaScript injection, normal browser cookie scraping or disk cookie export is permitted.

## Developer-only build

On an isolated Windows developer runner with .NET 8 SDK and WebView2 runtime:

```powershell
dotnet restore tools/muse-login-shell-poc/MuseLoginShellPoc.csproj
dotnet build tools/muse-login-shell-poc/MuseLoginShellPoc.csproj --configuration Release --no-restore
```

End users must not be required to install SDKs or run these commands. GitHub CI checks buildability only, not real sign-in or quota access. The main Rust monitor and Windows Store v1.4.12 are unchanged.

## Validation gates before considering a product feature

1. Explicit user consent and actual Windows WebView2 sign-in in a test environment, without touching browser credentials.
2. Verify logout and session-data reset as a distinct operation with actual Muse sign-in.
3. Determine a Meta-permitted, read-only **quota data** source; login alone is not proof.
4. Verify Windows packaging, account/plan identity, fresh/expired/zero/no-data handling, security, and free/paid Store release permission independently.


## 2026-11-07: opt-in passive quota response pilot

After the user signs into the **dedicated WebView2 profile**, they can choose
`設定画面の応答を取得（1回）` and open Muse's own
Settings → General → Usage view within 60 seconds. The experiment listens for
an already-issued first-party `muse.ai` POST response with a `Next-Action`
request header and `text/x-component` content type. It does **not** issue
a private action request, read cookies, decrypt browser credentials, or inject
JavaScript. Only a bounded response (64 KiB), in process memory, is checked
for `subscription.tier.name`, `usage.percentUsed` and `usage.resetsAt`;
other fields, raw responses, headers, and account data are not logged or saved.

The feature is **off by default**, single-shot, at most six candidates and
60 seconds. Unknown schemas or missing data show unavailable, **never 0%**.
Synthetic CI exercises the WebView2 response and exact parser. No real Muse
account is used in CI. On 2026-11-07 the user observed a real, single-shot 7% quota response matching the first-party Muse screen. The automatic path remains unverified on the real account. This code is research-only:
**NO production Rust integration, NO MSIX/Store distribution, NO public release.**
This pilot does not establish Meta's permission for public or commercial use.

## 2026-11-07: optional automatic self-only quota pilot

With the same dedicated WebView2 login profile already signed in, click
`Muse自動更新を開始（15分間隔）`. The PoC attempts an immediate read and then
a read every 15 minutes *only while the PoC remains open*. The toggle is
off at startup, not persisted, and has an explicit stop action.

The PoC temporarily obtains **only that WebView2 profile's first-party
muse.ai cookies** with the official WebView2 CookieManager. Its own HTTPS
client then requests the first-party page, discovers a current subscription
action ID from public static chunks, and makes the corresponding
same-origin subscription POST. The internal action ID is cached in process
memory only after valid results, with a single rediscovery on stale 404.
Cookies are sent only to muse.ai over HTTPS, not to static resources, and
requests have redirects disabled. The account cookie header and full
subscription response are never written to custom logs, files, clipboard,
telemetry or the production monitor.

A bounded synthetic HTTP transport test validates action discovery, cached
follow-up polling, 7%/reset parsing, cookie-free static requests, and signed-out
and rate-limited responses, but **does not prove real Muse automatic polling**.
Discovery uses at most 96 primary chunks, 32 settings chunks, an 8 MiB cap,
and one 45-second HTTP timeout per HTTP request; 401/403/429 stop the
automatic timer rather than looping. Other errors retain an unavailable
status without inventing 0% usage. The user must independently opt in before
any live automatic request.

This relies on an **undocumented private service action**. It could stop
working when Muse changes. Competitors using a similar endpoint do not
prove Meta permission. Personal-local tests are authorized by the user's
direction; public free/paid distribution, Store integration, production Rust
provider wiring and automatic deployment remain on HOLD.


## 2026-11-08: bounded local refresh status receipt (personal-only QA)

The user should not need to read a screenshot or communicate the last
successful fetch time. While the **opt-in automatic mode** is running, the
PoC writes a single atomic JSON receipt under the same Windows user's
`%LOCALAPPDATA%\\Solidaily\\MuseLoginShellPoc\\refresh-health.json`.
Only the following keys are persisted:
`schema_version`, `state`, `session_started_utc`,
`last_attempt_utc`, `previous_success_utc`, `last_success_utc`,
`success_count_in_session`, `last_outcome`, `recorded_utc`.

**No percentage, plan, reset date, subscription data, user ID, email,
cookie, session token, request URL, request/response body or exception
message is written.** Unknown errors are one generic status.
The status file is owned by the local user and is only intended for
an already-authorized *read-only* diagnostic channel to inspect.
No telemetry or cloud upload is added; no background task or unattended
execution is enabled on app startup. The receipt cannot prove that the
process is *still* running by itself—inspect the process separately.

A successful 15-minute repeat in the same opt-in session requires both
`success_count_in_session >= 2` and a later
`last_success_utc` than `previous_success_utc`; the old program cannot
retroactively generate this receipt. The deterministic synthetic CLI test
`--refresh-health-self-test` verifies atomic writes, stop/error,
two successes, session count reset, and exclusion of secret/quota terms.
