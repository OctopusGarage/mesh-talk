# Testing Guidelines

This document provides guidelines for writing and organizing tests in the Mesh-Talk project.

## Rust Backend Testing

### Module-Level Unit Tests

For small, focused tests of individual functions or methods, place tests directly in the same file as the code they're testing, using the `#[cfg(test)]` attribute:

```rust
// src-tauri/src/services/user.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_user() {
        let user = User::new("Alice");
        assert_eq!(user.name, "Alice");
    }
}
```

### Integration Tests

For tests that involve multiple modules or test the system as a whole, place them in the `src-tauri/tests/` directory:

```
src-tauri/
├── tests/
│   ├── node_service_test.rs
│   └── network_test.rs
```

Execute all tests with:

```bash
cd src-tauri && cargo test
```

## Frontend Testing

Vue frontend tests should use Vitest/Jest and can be organized in two ways:

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

Execute frontend tests with:

```bash
cd frontend && pnpm test
# or
cd frontend && npm run test
```

## Testing Best Practices

1. **Rust Tests**:
   - Use module-level tests for unit testing individual functions
   - Use integration tests (`src-tauri/tests/`) for cross-module functionality
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