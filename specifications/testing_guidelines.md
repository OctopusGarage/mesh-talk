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
`data-testid`), which drives the running app in a browser:

```bash
cd frontend && npm run e2e        # CI: .github/workflows/e2e-ui.yml
```

## Testing Best Practices

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