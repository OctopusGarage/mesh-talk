# Testing Guidelines

This document provides guidelines for writing and organizing tests in the Mesh-Talk project.

## Rust Backend Testing

### Module-Level Unit Tests

For small, focused tests of individual functions or methods, place tests directly in the same file as the code they're testing, using the `#[cfg(test)]` attribute:

```rust
// crates/mesh-talk-core/src/identity/keys.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_roundtrip() {
        let id = DeviceIdentity::generate();
        assert_eq!(id.user_id().len(), 32);
    }
}
```

Test builds use cheap KDF parameters (the `fast-test-kdf` feature, enabled via
`cfg(test)`), so the suite is not KDF-bound; `.cargo/config.toml` additionally caps
compile/test parallelism so a full run does not pin every core.

### Integration & multi-process E2E Tests

Cross-module and end-to-end suites live in `crates/mesh-talk-core/tests/`
(`two_node_cli`, `persistent_history`, `post_office_offline`, `decoder_smoke`); node
behaviour also has inline tests in `crates/mesh-talk-core/src/node/node_tests.rs`:

```
crates/mesh-talk-core/
├── tests/                         # integration + multi-process E2E suites
└── src/node/node_tests.rs         # node behaviour (unit + integration)
```

Run the workspace unit/integration suite:

```bash
cargo test --workspace        # or: make test
```

The multi-process backend E2E rigs spawn real `mesh-talk-node` processes over UDP
discovery + TCP and are `#[ignore]`d by default (slow cold starts); run them with:

```bash
make e2e        # CI: .github/workflows/e2e-backend.yml
```

## Frontend Testing

React frontend tests should use Vitest + Testing Library and can be organized in two ways:

1. Place tests alongside the components/modules they test:
   ```
   frontend/src/components/UserForm.spec.ts
   frontend/src/store/user.spec.ts
   ```

2. Or organize tests in a dedicated tests folder:
   ```
   frontend/tests/
   ├── components/
   │   └── UserForm.spec.ts
   └── store/
       └── user.spec.ts
   ```

Execute frontend unit tests (Vitest) with:

```bash
cd frontend && npm run test
```

The frontend also has a Playwright UI end-to-end suite (selectors keyed on
`data-testid`), which drives the real React app in Chromium with mocked Tauri IPC:

```bash
cd frontend && npm run e2e        # CI: .github/workflows/e2e-ui.yml
```

The suite checks messages and recovery paths, six themes and screenshot baselines,
large virtualized conversation/member lists, compact windows, touch targets, and
representative WCAG A/AA rules with axe. Browser automation does not replace testing
the native webview, real network, or a screen reader. Use
[`docs/chat-ui-manual-test.md`](../docs/chat-ui-manual-test.md) for the human smoke pass.

The detached [`fuzz/`](../fuzz/README.md) crate covers untrusted protocol decoders.
Compile its targets with `cargo check --manifest-path fuzz/Cargo.toml --bins`; nightly
`cargo-fuzz` is required for an instrumented run.

Run `cd frontend && npm run test:coverage` for a report over all production
TypeScript and TSX files. CI runs this on Linux, macOS and Windows and stores
the Linux report as an artifact. The store line threshold guards against a
regression in state logic; interactive components are exercised separately by
Playwright and native desktop E2E. A zero in the Vitest report is a unit-test
gap, not proof that the component has no browser coverage.

## Delivery / AI Regression Smoke

The project has a deterministic smoke gate for delivery verification and AI-agent prompt
contracts:

```bash
make eval-smoke
```

The real model-backed eval is:

```bash
AI_EVAL_COMMAND='<non-interactive model command>' make ai-eval
```

The complete real smoke suite is:

```bash
make smoke-full
```

Run `make eval-smoke` when changing code, prompts, model configuration, agent rules, workflow
scripts, CI, or core protocol flows. Run `make ai-eval` for prompt/model/agent workflow
changes. Run `make smoke-full` for release-critical changes or broad workflow changes.

## Testing Best Practices

See [quality gates and release evidence](../docs/evals/quality-gates.md) for the
required scenario matrix, failing-baseline evidence, mutation-result semantics and
native platform limitations. Record applicable gaps in the PR; line coverage alone
does not establish correctness.

1. **Rust Tests**:
   - Use module-level tests for unit testing individual functions
   - Use integration tests (`crates/mesh-talk-core/tests/`) for cross-module functionality
   - Name tests descriptively to indicate what is being tested
   - Use assertions to verify expected behavior
   - Mock external dependencies when possible

2. **Frontend Tests**:
   - Test component logic separately from UI rendering
   - Use shallow mounting for unit tests
   - Test user interactions and state changes
   - Mock API calls and external dependencies
   - Use snapshot testing for UI components when appropriate

3. **General Principles**:
   - Tests should be fast and isolated
   - Tests should be deterministic (same input always produces same output)
   - Tests should be readable and maintainable
   - Tests should cover both happy paths and error cases
   - Tests should be run regularly as part of the development workflow
