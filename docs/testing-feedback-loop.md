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
