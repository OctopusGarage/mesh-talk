# Script entry points

Choose the smallest command that answers the current question. During ordinary
development, run affected unit or integration tests using the examples in
[`docs/testing-feedback-loop.md`](../docs/testing-feedback-loop.md). The entries
below are explicit gates or environment-specific tools.

| Purpose                      | Entry point                                                                                        | When to run                                                                                   |
| ---------------------------- | -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Focused PR review            | [`pr-preflight.sh`](pr-preflight.sh)                                                               | After committing, with the relevant Playwright specs                                          |
| Comprehensive health         | [`check-health.sh`](check-health.sh)                                                               | Before requesting review or in CI; `--fast` is an optional partial check                      |
| Delivery and agent contracts | [`ai-eval-smoke.sh`](ai-eval-smoke.sh), [`ai-eval.mjs`](ai-eval.mjs)                               | When changing their contracts or in CI; the model-backed runner needs a provider              |
| Complete real smoke          | [`run-real-smoke.sh`](run-real-smoke.sh)                                                           | Explicit release validation; includes backend and browser E2E                                 |
| Git hooks                    | [`setup-hooks.sh`](setup-hooks.sh), [`../hooks/`](../hooks/)                                       | On initial checkout; the commit hook checks source formatting only                            |
| Cargo cache                  | [`clean-target-cache.sh`](clean-target-cache.sh), [`auto-clean-targets.sh`](auto-clean-targets.sh) | Explicit maintenance; the macOS scheduler is installed by `make dev`                          |
| Release                      | [`release/`](release/)                                                                             | CI release workflow and signed artifact validation                                            |
| CI diagnostics               | [`diagnostics/`](diagnostics/) and [`evals/`](evals/)                                              | Their `.test.mjs` files are selected by CI globs, even when no script names them individually |

## Opt-in network acceptance

These scripts use disposable profiles and test-only binaries. The script header
owns exact builds/hosts/flags; [network evidence](../docs/network-design-reference.md#implementation-and-evidence-tracker)
owns what each observed result establishes. Windows test data, profiles, working
directories and temporary files belong on drive D.

| Rig                                        | Distinct boundary                                                                                                |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------- |
| `e2e-tailnet-macos.py`                     | Direct routed discovery, bidirectional DMs and consented file integrity on two Macs.                             |
| `e2e-tailnet-bridge.py`                    | One bridge introduces clients across isolated discovery scopes.                                                  |
| `e2e-tailnet-two-bridges.py`               | Legacy reciprocal gateway compatibility; not equal-federation acceptance.                                        |
| `e2e-four-machine-tailnet.py`              | Four-host gateway/federation/installed-relay modes and consented cross-site files.                               |
| `e2e-four-machine-hub.py`                  | Explicit selected-hub compatibility with another local relay; not a mandatory global-hub architecture.           |
| `e2e-post-office-lan.py`                   | Directly consented offline custody/restart and optional mailbox-only local alternative.                          |
| `e2e-federation-recovery.py`               | Preferred outage, surviving three-relay chain, file completion after sender restart and partition/rejoin.        |
| `e2e-routed-sibling.py`                    | Actual SDK pairing/restarts, controlled history sharing and ongoing sibling replay over Tailscale.               |
| `frontend/e2e/call-media-physical.spec.ts` | Opt-in real Noise signaling and Tailscale ICE/DTLS browser media; separate from native WebView/hardware quality. |

The legacy rigs remain because they test distinct compatibility contracts. They
are not routine local tests and do not authorize changes to installed services.
The [testing workflow](../docs/testing-feedback-loop.md#explicit-physical-network-acceptance)
explains selection and the public SDK acceptance host.

## Maintenance policy

Release verification lives in `release/`; quality gates live in their maintained
entry points above. Reuse these owners instead of adding wrappers with copied
command lists. A script without direct imports may still be selected by CI globs
or be an explicitly documented manual acceptance rig; check those callers before
removing it. Dated evidence is retained as evidence, not rewritten as current
implementation truth.
