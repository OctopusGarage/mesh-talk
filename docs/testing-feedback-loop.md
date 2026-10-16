# Local test feedback loop

During feature development, run the smallest tests that cross the changed
boundary. The pre-commit hook checks staged whitespace and source formatting;
it does not launch test suites. Record the focused commands and results in the
PR. Before opening a PR, run `make test` and `./scripts/pr-preflight.sh` with
the relevant Playwright specs. Run `./scripts/check-health.sh` before requesting
review. CI also runs the comprehensive suites. Run backend or full UI E2E sooner
when explicitly requested or when the changed behavior needs process/browser
evidence.

| Changed boundary            | Fast local command                                                                                          | Broader trigger                                             |
| --------------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| Core unit module            | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync`                              | `make test` before PR                                       |
| Host settings/session       | `make test-focused TEST_PACKAGE=mesh-talk TEST_FILTER=owner_scoped_node_handle`                             | `make test` before PR                                       |
| One Rust integration target | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_TARGET=decoder_smoke`                                   | `make e2e` when requested or for release validation         |
| Frontend state/controller   | `cd frontend && npm test -- src/features/chat/settingsDialogSession.test.ts`                                | `npm run test:coverage` in PR preflight                     |
| Frontend rendering          | Run the affected component test, for example `cd frontend && npm test -- src/features/chat/Sidebar.test.ts` | Pass relevant Playwright specs to `scripts/pr-preflight.sh` |

Use `cargo test -p <package> --lib <module_or_test_name>` directly when a
filter is easier to spell than a Make variable. `make test-focused` requires a
package plus either a filter or an integration target, so an omitted selector
cannot silently start the whole workspace. Ignored process tests require an
explicit `cargo test ... -- --ignored --test-threads=1`; the focused target does
not enable them automatically.

## Automatic platform checks before PR creation

Commit the candidate and use the repository's PR entry point:

```bash
./scripts/create-pr.sh \
  --core-filter node::delivery_runtime_tests \
  --host-filter owner_scoped_node_handle \
  e2e/ui-visual-regression.spec.ts \
  -- --draft --fill
```

Arguments before `--` select tests; arguments after it go to `gh pr create`.
Use filters and browser specs relevant to your change. Core and host filters
can be repeated; a changed Rust crate requires at least one corresponding
filter. Use `--core-test decoder_smoke` to select a nonignored core integration
target instead of a library filter. A selector matching zero tests fails rather than producing misleading
evidence. This selects unit tests and explicitly named browser scenarios; it
does not automatically launch the full workspace, ignored process rigs, or
the full browser suite.

Frontend changes require affected browser specs. Frontend unit coverage runs
once in the existing local preflight; the two platform guests do not repeat
the whole frontend unit suite. To target another PR base, pass
`--base origin/release` before `--`; the wrapper uses that same base for
change selection, coverage and `gh pr create --base release`. A local revision
such as `HEAD` cannot stand in for a PR target branch.

`scripts/pr-preflight.sh` automatically sends the same test plan to Linux
Docker and Windows over SSH before its existing coverage and portable browser
checks. Both platforms test an archive of committed HEAD. Workspaces are
isolated from installed applications and post-office services. Both results
are collected even if one fails. Logs and revision-bound reports remain
untracked under `target/pr-platform/`; dependency and build caches can be
reused, but successful verdicts are never reused for another revision.
Windows reuses an installed Cargo home only when its resolved physical
directory is on drive D, including a C: junction that points to D. This
preserves that directory's registry and network configuration without logging
its contents. Otherwise, Cargo uses the isolated D: cache. Source, build
output, browser downloads and temporary directories remain isolated.

For an existing PR, run `scripts/pr-preflight.sh` directly with the same
selectors before pushing the candidate. Preview the plan without starting
Docker or SSH:

```bash
node scripts/pr-platform/run.mjs --plan \
  --core-filter node::delivery_runtime_tests \
  --host-filter owner_scoped_node_handle \
  e2e/ui-visual-regression.spec.ts
```

| Setting                            | Default / purpose                                                                                            |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `MESH_PR_WINDOWS_HOST`             | `windows10`; an SSH host with noninteractive access, Rust, Node and Git tools available                      |
| `MESH_PR_WINDOWS_ROOT`             | `D:/MeshTalk/PRPreflight`; Windows source, caches, browser downloads and temporary data must stay on drive D |
| `MESH_PR_DOCKER_IMAGE`             | Optional prepared image override; the default image is built from `scripts/pr-platform/Dockerfile`           |
| `MESH_PR_PLATFORM_TIMEOUT_SECONDS` | `1800`; platform execution limit, not a protocol timeout                                                     |
| `PATCH_COVERAGE_BASE` / `--base`   | `origin/main`; the comparison base for changed-file planning                                                 |

Required environments being unavailable blocks preparation; it is not a pass.
Fix the environment or report the blocker. Documentation-only changes without
explicit selectors report platform tests as not applicable. Container reports state the actual
architecture: Linux ARM64 on an Apple Silicon Mac does not replace Linux
x86-64 sanitizer coverage. Browser checks exercise the real frontend with its
test IPC fixture. Native webview, desktop input, multicast across real hosts,
and ASan remain separate CI or explicitly requested acceptance gates.

The PR creation wrapper also checks that HEAD and the working tree did not
change during verification. Alternate head/repository options and `GH_REPO`
overrides are rejected. Use it instead of invoking `gh pr create` directly
so the platform checks are part of the normal workflow. GitHub branch
protection remains the final enforcement for CI; a local script cannot
intercept arbitrary GitHub CLI or website actions.

The current two-crate Rust workspace already separates protocol logic from
the Tauri host. Within the core, `node/node.rs` still coordinates many domain
modules; keep moving bounded parsing and state transitions behind narrow
module interfaces so `--lib <module>` reaches them. In the host, ownership
validation belongs to `NodeState`; commands should use its owner-scoped handle
instead of repeating runtime locks and session comparisons. In the frontend,
the Settings dialog's session controller and store tests provide narrow targets;
continue extracting field-specific state transitions from the presentation
component as those fields change. Prefer tests at those interfaces over extra
process/browser cases for pure state rules.

For local speed, keep the existing Cargo build cache and the test-only cheap
KDF parameters. Avoid invoking Clippy, full Rust tests, coverage, and browser
tests after every small edit; run the focused test during the loop, then the
appropriate static check and final gates at the triggers above. Do not change
the CI commands or the explicit `make test`, `make e2e`, and health-check
entries: they provide the comprehensive evidence for review and release.

## Explicit physical network acceptance

These opt-in checks use disposable profiles and test-only `fast-test-kdf`
binaries. Do not deploy those binaries as permanent services. They require the
four authorized SSH hosts and their actual Tailscale routes; failure to reach a
host is not a protocol pass. Windows fixtures, working directories, profiles and
temporary files belong on drive D. No installed relay or network service needs
to be stopped.

- `scripts/e2e-routed-sibling.py`: build `network-acceptance` for arm64 and Intel
  macOS, then exercise real SDK pairing, restart, history sharing off/on and
  continued catch-up across mac2015 and mac-mini.
- `scripts/e2e-federation-recovery.py`: build `mesh-talk-node` for arm64 macOS,
  Intel macOS and Windows GNU, plus the arm64 `network-acceptance` example.
  Exercise preferred-relay outage, a surviving three-relay route, consented file
  delivery with sender restart and SHA-256, and bridge partition/recovery.
- `frontend/e2e/call-media-physical.spec.ts`: build the `network-acceptance`
  example for native macOS and Windows GNU, then run
  `MESH_TALK_PHYSICAL_MEDIA=1 npx playwright test e2e/call-media-physical.spec.ts`
  from `frontend/`. Override `MESH_TALK_MEDIA_LOCAL_IP` and
  `MESH_TALK_MEDIA_REMOTE_IP` with the actual directly reachable Tailscale IPs.
  It uses shared browser media
  fixtures, authenticated backend signaling and an independently checked
  Tailscale ICE pair between Chromium and Windows Edge. Synthetic capture
  verifies transport/decode and cleanup, not native WKWebView or camera quality.

Set `MESH_TALK_E2E_MAC2015_SSH=ys-aquria@192.168.3.72` when the `.local` alias
is unavailable; `MESH_TALK_E2E_BASE_PORT` isolates disposable port ranges. Keep
logs and machine-specific evidence outside git. Four hosts at two physical
sites cannot establish a three-independent-LAN acceptance claim.
