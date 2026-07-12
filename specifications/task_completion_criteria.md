# Task Completion Criteria

A task is **done** only when all of the following hold:

1. **Implementation** — the change matches its design (see `docs/ARCHITECTURE.md` and the
   relevant plan/spec under `docs/superpowers/`), follows
   [`development_conventions.md`](development_conventions.md), and touches only what the
   task requires.
2. **Tests** — unit + integration tests cover the normal and failure paths; the **full
   suite passes** (`cargo test`). Crypto/networking changes ship a loopback integration
   assertion.
3. **The gate is green** — `make eval-smoke` and `./scripts/check-health.sh` pass end-to-end: `cargo fmt`,
   `cargo clippy --all-targets -- -D warnings`, ESLint, `cargo test`, typos, cargo-deny,
   cargo-machete, gitleaks, shellcheck, security audits, and both builds. CI mirrors this
   (the `verify` check is required by branch protection).
   Prompt, model, agent-rule, or eval-suite changes also require `make ai-eval` with a real
   model provider. Release-critical flow changes require `make smoke-full` or a documented
   CI run of the equivalent backend/UI E2E workflows.
4. **Docs** — any affected doc is updated (`docs/ARCHITECTURE.md` for architecture; the
   relevant `docs/superpowers/` spec/plan; this `specifications/` set for conventions).
5. **Security** — no new advisory (cargo-deny/audit clean), no hard-coded secrets
   (gitleaks clean), and at-rest/in-transit data stays encrypted.

> The full check-health gate includes the delivery / AI eval smoke gate. If it passes and
> the docs are updated, the task is complete.
