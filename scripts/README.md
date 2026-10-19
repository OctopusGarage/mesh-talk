# Script entry points

Choose the smallest command that answers the current question. During ordinary
development, run affected unit or integration tests using the examples in
[`docs/testing-feedback-loop.md`](../docs/testing-feedback-loop.md). The entries
below are explicit gates or environment-specific tools.

| Purpose                      | Entry point                                                          | When to run                                                                                                                       |
| ---------------------------- | -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| PR preparation               | [`prepare-pr.sh`](prepare-pr.sh)                                     | Full health and platform preflight before creating or updating a PR                                                               |
| Focused PR review            | [`pr-preflight.sh`](pr-preflight.sh)                                 | After committing, with the relevant Playwright specs                                                                              |
| Verified PR creation         | [`create-pr.sh`](create-pr.sh)                                       | Runs full preparation, then creates the PR only for the verified candidate                                                        |
| Native build evidence        | [`validation/native-build.mjs`](validation/native-build.mjs)         | Build source-pinned desktop acceptance binaries before producing native reports                                                   |
| Linux / Windows preflight    | [`pr-platform/run.mjs`](pr-platform/run.mjs)                         | Automatic in PR preflight; `--plan` previews selections without running environments                                              |
| Comprehensive health         | [`check-health.sh`](check-health.sh)                                 | Before requesting review or in CI; `--fast` is an optional partial check                                                          |
| Delivery and agent contracts | [`ai-eval-smoke.sh`](ai-eval-smoke.sh), [`ai-eval.mjs`](ai-eval.mjs) | When changing their contracts or in CI; the model-backed runner needs a provider                                                  |
| Complete real smoke          | [`run-real-smoke.sh`](run-real-smoke.sh)                             | Explicit release validation; includes backend and browser E2E                                                                     |
| Git hooks                    | [`setup-hooks.sh`](setup-hooks.sh), [`../hooks/`](../hooks/)         | On initial checkout; the commit hook checks source formatting only                                                                |
| Build retention              | [`build-retention.py`](build-retention.py)                           | After managed builds/tests; five successful build sets and a 40 GiB cache budget; see [configuration](../docs/build-retention.md) |
| Release                      | [`release/`](release/)                                               | CI release workflow and signed artifact validation                                                                                |
| CI diagnostics               | [`diagnostics/`](diagnostics/) and [`evals/`](evals/)                | Their `.test.mjs` files are selected by CI globs, even when no script names them individually                                     |

## Opt-in network acceptance

These scripts use disposable profiles and test-only binaries. The script header
owns exact builds/hosts/flags; [network evidence](../docs/network-design-reference.md#implementation-and-evidence-tracker)
owns what each observed result establishes. Windows test data, profiles, working
directories and temporary files belong on drive D.

| Rig                                        | Distinct boundary                                                                                                        |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| `e2e-tailnet-macos.py`                     | Direct routed discovery, bidirectional DMs and consented file integrity on two Macs.                                     |
| `e2e-easytier-macos.py`                    | EasyTier automatic desktop inventory/discovery, bidirectional DMs/group events and consented file integrity on two Macs. |
| `e2e-tailnet-bridge.py`                    | One bridge introduces clients across isolated discovery scopes.                                                          |
| `e2e-tailnet-two-bridges.py`               | Legacy reciprocal gateway compatibility; not equal-federation acceptance.                                                |
| `e2e-four-machine-tailnet.py`              | Four-host gateway/federation/installed-relay modes and consented cross-site files.                                       |
| `e2e-four-machine-hub.py`                  | Explicit selected-hub compatibility with another local relay; not a mandatory global-hub architecture.                   |
| `e2e-post-office-lan.py`                   | Directly consented offline custody/restart and optional mailbox-only local alternative.                                  |
| `e2e-federation-recovery.py`               | Preferred outage, surviving three-relay chain, file completion after sender restart and partition/rejoin.                |
| `e2e-routed-sibling.py`                    | Actual SDK pairing/restarts, controlled history sharing and ongoing sibling replay over Tailscale.                       |
| `frontend/e2e/call-media-physical.spec.ts` | Opt-in real Noise signaling and Tailscale ICE/DTLS browser media; separate from native WebView/hardware quality.         |

The legacy rigs remain because they test distinct compatibility contracts. They
are not routine local tests and do not authorize changes to installed services.
The [testing workflow](../docs/testing-feedback-loop.md#explicit-physical-network-acceptance)
explains selection and the public SDK acceptance host.

## Maintenance policy

Release verification lives in `release/`; quality gates live in their maintained
entry points above. Reuse these owners instead of adding wrappers with copied
command lists. A script without direct imports may still be selected by CI globs
or be an explicitly documented manual acceptance rig; check those callers before
removing it. Keep scratch probes under ignored `tmp/`; remove them after the task. Store
executed validation evidence in PR/CI artifacts. Update the owning guide when
behavior changes instead of adding dated implementation reports.

## Local validation stages

`validation/stage.mjs` runs named commands with explicit argv, stage timings,
logs and successful local evidence reuse. Example:

```bash
node scripts/validation/stage.mjs workspace-tests --profile shared --resource heavy -- cargo test --workspace
MESH_VALIDATION_FRESH=1 ./scripts/check-health.sh
```

Reuse requires a clean identical revision, matching tool/environment/command
inputs, and intact required artifacts; receipts expire after 24 hours. CI always
executes. Shared stages exclude PR transport settings from identity because those
settings do not affect local checks. Full preflight binds its comparison base and
always executes Linux/Windows checks. Resource locks serialize heavy workloads
across worktrees. `validation/selectors.mjs` checks actual compiled test inventories
before expensive browser/remote work; `validation/browser.mjs` owns each browser
run's port and evidence directory. Neither helper removes required scenarios.

## Repository hygiene

`node scripts/check-repository-hygiene.mjs` rejects ignored files in the whole Git
index, including force-added files. It shares `.gitignore` with pre-commit, CI and
the health gate. Its focused regression command is
`node --test scripts/diagnostics/repository-hygiene.test.mjs`.
