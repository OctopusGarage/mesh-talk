# Git Commit Standards for Mesh-Talk

To maintain clear and consistent commit history, we follow professional Git commit standards.

## Commit Message Format

```
<type>(<scope>): <subject>

<body>

<footer>
```

## Commit Types

- `feat`: New feature
- `fix`: Bug fix
- `bug`: Bug fix (synonymous with `fix`)
- `docs`: Documentation update
- `style`: Code formatting adjustment
- `refactor`: Code refactoring
- `perf`: Performance optimization
- `test`: Test-related
- `build`: Build system or external dependency changes
- `ci`: CI configuration files and script changes
- `chore`: Other changes that don't modify src or test files
- `revert`: Rollback previous commit

## Scope

Scope should identify the component or module affected by the commit:

- `network`
- `crypto`
- `ui`
- `db`
- `core`
- `cli`
- `tests`
- `config`

## Commit Message Standards

1. Write commit messages in English
2. First line is a brief description (<72 characters)
3. Second line must be a genuine blank line
4. Third line onwards is detailed description (optional)
5. Detailed description can include change reasons, impact scope, and other information
6. Make sure not to include temporary files generated during builds

## Examples

```
feat(discovery): sign UDP announce and group devices by account

- Add Ed25519 signature over the announce payload
- Carry the device certificate and verify it against the account key
- Group discovered devices by account in the roster
```

```
fix(ratchet): resolve session establishment failure

- Fix race condition in double ratchet initialization
- Add proper error handling for key exchange failures
- Update unit tests to cover edge cases
```

## Signing & Hooks

Commits **must be GPG-signed**. `scripts/setup-hooks.sh` (run by `make dev`) points
`core.hooksPath` at `hooks/` and enables `commit.gpgsign`; the `pre-push` hook rejects any
commit lacking a `gpgsig`. The `commit-msg` and `pre-commit` hooks run a fast quality slice
locally before each commit. Generate a key with `gpg --full-generate-key` and set it via
`git config --global user.signingkey <KEY_ID>` if you don't have one.