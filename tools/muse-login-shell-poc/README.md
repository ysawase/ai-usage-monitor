# Muse Windows WebView2 login shell — isolated PoC

**LOGIN-ONLY EXPERIMENT. NOT A MUSE QUOTA INTEGRATION. NOT FOR STORE DISTRIBUTION.**

This minimal .NET 8 WinForms/WebView2 application opens the first-party personal Muse website at `https://muse.ai/` in an app-specific profile. It does not use browser extensions, read the normal browser's cookies, execute script in Muse, call private server actions, read subscription quota, or send data to the monitor.

- Dedicated local profile: `%LOCALAPPDATA%\\Solidaily\\MuseLoginShellPoc\\WebView2Profile` (not Chrome/Opera/Edge profiles)
- Navigate only to `https://muse.ai` and `https://auth.muse.ai`, plus `about:blank` for local reset
- Block popups, WebMessage, DevTools, host objects, and browser permission prompts
- An explicit confirmation clears browsing data **only** in this experimental profile; the code never deletes the regular browser profile or user's Muse account
- No credentials or raw webpage response are logged, copied, or stored by custom code. The WebView2 runtime itself persists normal browser session state in the isolated profile.

The demo may be unable to finish authentication if Muse uses additional login domains, popups or provider-specific flows. Do not relax the allowlist without reviewing actual first-party login requirements and consent. No JavaScript injection or cookie-read fallback is permitted.

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
account is used in CI. The live server's actual response format and support for
this passive technique remain unverified. This code is research-only:
**NO production Rust integration, NO MSIX/Store distribution, NO public release.**
This pilot does not establish Meta's permission for public or commercial use.
