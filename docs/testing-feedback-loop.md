# Local test feedback loop

During feature development, run the smallest tests that cross the changed
boundary. The pre-commit hook checks staged whitespace and source formatting;
it does not launch test suites. Record the focused commands and results in the
PR. Before opening a PR, run `make test` and `./scripts/pr-preflight.sh` with
the relevant Playwright specs. Run `./scripts/check-health.sh` before requesting
review. CI also runs the comprehensive suites. Run backend or full UI E2E sooner
when explicitly requested or when the changed behavior needs process/browser
evidence.

| Changed boundary | Fast local command | Broader trigger |
|---|---|---|
| Core unit module | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync` | `make test` before PR |
| Host settings/session | `make test-focused TEST_PACKAGE=mesh-talk TEST_FILTER=owner_scoped_node_handle` | `make test` before PR |
| One Rust integration target | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_TARGET=decoder_smoke` | `make e2e` when requested or for release validation |
| Frontend state/controller | `cd frontend && npm test -- src/features/chat/settingsDialogSession.test.ts` | `npm run test:coverage` in PR preflight |
| Frontend rendering | Run the affected component test, for example `cd frontend && npm test -- src/features/chat/Sidebar.test.ts` | Pass relevant Playwright specs to `scripts/pr-preflight.sh` |

Use `cargo test -p <package> --lib <module_or_test_name>` directly when a
filter is easier to spell than a Make variable. `make test-focused` requires a
package plus either a filter or an integration target, so an omitted selector
cannot silently start the whole workspace. Ignored process tests require an
explicit `cargo test ... -- --ignored --test-threads=1`; the focused target does
not enable them automatically.

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
