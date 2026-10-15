# Script entry points

Choose the smallest command that answers the current question. During ordinary
development, run affected unit or integration tests using the examples in
[`docs/testing-feedback-loop.md`](../docs/testing-feedback-loop.md). The entries
below are explicit gates or environment-specific tools.

| Purpose | Entry point | When to run |
|---|---|---|
| Focused PR review | [`pr-preflight.sh`](pr-preflight.sh) | After committing, with the relevant Playwright specs |
| Comprehensive health | [`check-health.sh`](check-health.sh) | Before requesting review or in CI; `--fast` is an optional partial check |
| Delivery and agent contracts | [`ai-eval-smoke.sh`](ai-eval-smoke.sh), [`ai-eval.mjs`](ai-eval.mjs) | When changing their contracts or in CI; the model-backed runner needs a provider |
| Complete real smoke | [`run-real-smoke.sh`](run-real-smoke.sh) | Explicit release validation; includes backend and browser E2E |
| Git hooks | [`setup-hooks.sh`](setup-hooks.sh), [`../hooks/`](../hooks/) | On initial checkout; the commit hook checks source formatting only |
| Cargo cache | [`clean-target-cache.sh`](clean-target-cache.sh), [`auto-clean-targets.sh`](auto-clean-targets.sh) | Explicit maintenance; the macOS scheduler is installed by `make dev` |
| Release | [`release/`](release/) | CI release workflow and signed artifact validation |
| CI diagnostics | [`diagnostics/`](diagnostics/) and [`evals/`](evals/) | Their `.test.mjs` files are selected by CI globs, even when no script names them individually |

The `e2e-*.py` scripts are manual real-machine rigs for LAN, Tailscale, bridge,
and post-office scenarios. Their required hosts and limitations are in
[`docs/network-topologies.md`](../docs/network-topologies.md) and the
corresponding deployment guides. They are not routine local tests.

The release workflow uses `release/verify-release.sh` and
`release/verify_assets.py` for current archives. The old root-level release
verifier and automatic maintenance wrapper were removed because they duplicated
maintained entry points and had no callers. An unreferenced commit-message
rewriter was removed as well; normal commits use Git hooks and conventional
messages directly.
