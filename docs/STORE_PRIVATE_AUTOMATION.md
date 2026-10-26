# Microsoft Store self-only update automation

Workflow: `.github/workflows/store-private-package.yml`.
Scope: create an unsigned MSIX with identity `Solidaily.SolidailyAIUsageMonitor` for Private audience only. The workflow never submits to the Store, changes audience, publishes a GitHub Release, or updates WinGet.

## Before dispatch

- The workflow must first be merged into the default branch to be reliably available for manual workflow dispatch.
- The matching Store packaging script is on `feature/store-private-checkpoint`, not yet in `main`; integrate it carefully before using `main`.
- Select only a reviewed source ref containing exactly the approved fixes. The local L15 MSIX 1.4.11.0 has not automatically become part of this remote branch.
- Start the workflow manually, enter the approved numeric version (e.g. `1.4.11`), check tests, package identity, checksum and artifact contents.

## Separate Store submission: not yet enabled

The Store Developer CLI requires an Entra ID tenant and authorized Partner Center service principal (not the consumer Microsoft account). Store the tenant, seller, client IDs and client secret as protected GitHub environment secrets, never in repository files or workflow parameters.

First confirm read-only authentication and verify that Product ID `9MZRWSD4MTGC` is still Private audience / self-only. Then wire a separately approved submission job with an enforced reviewer environment (subject to plan/visibility support) and draft/readback checks. Do not call `msstore publish` unconditionally: it can replace an existing draft. `--noCommit` is not a dry run.

Official docs:
- https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/overview
- https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/github-actions
- https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/commands

Restrictions: no general public distribution, tag-release automation, accidental WinGet publication, automatic Store submission or credential leaks.

## Tenant setup check (2026-10-27 JST)

Read-only Partner Center browser inspection: `Account settings > Organization profile > Tenants > Developer` showed **Current tenant associations: No data available**. Choosing `Create Microsoft Entra ID` opened the Microsoft for your business signup (`signup.microsoft.com/createaccount?scenario=PartnerCenter`), which requests a work/school email and shows an account / sign-in / payment sequence. No tenant was created, no billing details entered, and no account permissions changed.

Microsoft currently states that **Microsoft Entra ID Free has no product charge but requires a credit card for identity verification**. The Partner Center work-account signup requires a legitimate accessible business email (not an Outlook.com personal address), business information and payment-method entry. This is a one-time human-managed account/billing/privilege boundary, not an automated CLI step. Never claim it is card-free or bypass it. If the user has an existing qualifying work/Entra account, prefer association over another tenant.

- https://learn.microsoft.com/en-us/azure/cost-management-billing/manage/microsoft-entra-id-free
- https://learn.microsoft.com/en-us/partner-center/enroll/create-work-account
- https://learn.microsoft.com/en-us/windows/apps/publish/partner-center/associate-existing-azure-ad-tenant-with-partner-center-account

**No-auth fallback:** Continue producing and validating a single unsigned MSIX via the manual-only GitHub workflow or L15 build. For each approved self-only release, use Partner Center's existing Store submission UI to upload that MSIX; certification supplies the Store-signed package. This reduces the repeat build steps without creating a new tenant, but upload/submit remains manual until the required Entra setup is authorized and validated. Do not sideload the unsigned MSIX as a replacement for the Store-signed app. Do not change Private audience or install/delete existing app packages as a shortcut.
