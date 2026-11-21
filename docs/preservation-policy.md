# Accepted-QA preservation gate (release safety)

Current baseline status: **UNVERIFIED / HOLD**. The old working QA EXE is retained; this repository cannot claim it was built from the current main commit. No new EXE may replace or supersede it on this evidence.

## Release preconditions

- Independently establish the accepted EXE/source provenance from original build artifacts, accepted features and local changes; record evidence in the specialist project current state.
- Record verified source commit, accepted executable SHA-256 and specific deterministic acceptance-test selectors in `docs/preservation-baseline.json`. Never promote placeholders merely to satisfy CI.
- Run `scripts/verify-preserved-baseline.ps1` before release build/publish. It rejects an unverified lineage, missing acceptance tests, dirty source, or failed feature-specific tests.
- After code gate PASS, separately verify old/new GUI feature parity and actual Muse / clipboard behavior. This script cannot prove GUI parity or stop a manual copy of an EXE.

The initial `UNVERIFIED` manifest **intentionally blocks releases** until the specialist project verifies its facts. Do not infer status VERIFIED from compilation or the count of Rust unit tests alone.
