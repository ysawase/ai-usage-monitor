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
