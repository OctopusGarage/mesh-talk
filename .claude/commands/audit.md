---
description: Audit Mesh-Talk — quality gates, dependency vulnerabilities, and secret scan
argument-hint: "[quick|full]"
allowed-tools: Bash, Read
---

Run the project's quality and security gates and summarize findings. Mirrors what
CI enforces (`.github/workflows/ci.yml`, `gitleaks.yml`).

Argument: `$ARGUMENTS` — `quick` (default) or `full`.

> Always prefix cargo with `nice -n 10` and throttle tests
> (`-- --test-threads=2`); the `.claude` CPU guard enforces this.

## quick (default)

- `nice -n 10 cargo fmt --all -- --check` — formatting (whole workspace).
- `nice -n 10 cargo clippy --workspace --all-targets -- -D warnings` — lints (both crates).
- `nice -n 10 cargo test --workspace -- --test-threads=2` — tests (both crates).

## full

Everything in `quick`, plus (mirrors `ci.yml`):

- **Supply chain**: `cargo deny check` — advisories + licenses + bans + sources
  (config `deny.toml`). Fixes go via `cargo update -p <crate>`, not by widening
  the ignore list. Brew: `brew install cargo-deny`.
- **Unused deps**: `cargo machete` (brew has no formula; `cargo install
  cargo-machete`) — the knip equivalent.
- **Coverage**: `cd src-tauri && nice -n 10 cargo llvm-cov --workspace
  -- --test-threads=2` (`cargo install cargo-llvm-cov`).
- **Spelling**: `typos` (config `_typos.toml`; `brew install typos-cli`).
- **Frontend**: `cd frontend && npm run lint && npm audit --audit-level=high`.
- **Secrets**: `gitleaks git . --config .gitleaks.toml --redact -v`.
- **Mutation** (slow): trigger the `Mutation Testing` workflow, or locally
  `cd src-tauri && cargo mutants --in-diff <(git diff origin/main...HEAD)`.

## Report

List each gate as ✅/❌ with the key finding. For any failure, name the file and
the specific issue. Do not claim "all clear" unless every gate you ran passed —
quote the command output.
