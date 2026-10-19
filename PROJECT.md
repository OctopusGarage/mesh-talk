# Mesh Talk project context

Mesh Talk is a local-first, end-to-end encrypted desktop messenger with a UI-free
Rust core, a Tauri host and a React frontend. Direct LAN/routed communication is
independent of optional peer-operated post offices. Equal trusted relays add
bounded encrypted custody and opt-in cross-site introductions; no global hub or
vendor-operated account service is required.

## Maintained references

| Question                                                | Reference                                                                                 |
| ------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Product overview and first run                          | [README](README.md)                                                                       |
| Communication principles, topology and module contracts | [Network architecture baseline](docs/network-design-reference.md)                         |
| Crypto, protocol, wire compatibility and storage        | [Architecture](docs/ARCHITECTURE.md)                                                      |
| Domain terminology and DDD conventions                  | [Canonical glossary](CONTEXT.md) · [DDD guide and reference index](docs/domain-design.md) |
| Current capabilities and acceptance boundaries          | [Implementation status](specifications/TODO.md)                                           |
| Development commands, agent rules and PR gates          | [Repository guidelines](AGENTS.md) · [Contributing](CONTRIBUTING.md)                      |
| Focused tests and escalation triggers                   | [Testing feedback loop](docs/testing-feedback-loop.md)                                    |
| Frontend interaction and visual conventions             | [Visual system](frontend/DESIGN.md)                                                       |
| All maintained guides and evaluation inputs             | [Documentation map](docs/README.md)                                                       |

## Rules to preserve

- Keep shared policy in its owning core module; UI/IPC consume its results.
- Separate discovery, direct reachability, authorization, relay custody and recipient confirmation.
- Preserve full device/account bindings, cryptographic domains and versioned wire contracts.
- Run affected tests during development; reserve comprehensive gates for explicit requests, PR/review and CI.
- Update the owning guide when behavior changes. Personal plans and temporary evidence remain untracked.

This entry point provides navigation. Detailed feature inventories, technology
versions and implementation history belong to their references and Git history.
