# Bot integrations: focused implementation evidence

Date: 2026-10-19 Asia/Tokyo. Base: `762525d`. Branch:
`feat/bot-mcp-architecture`. Platform: macOS ARM64, Rust 1.95.

This records the initial local text/reply release, not a full release gate. The
[usage guide](../bot-integrations.md) owns setup/current limits; the
[architecture proposal](../bot-mcp-architecture-research.md) distinguishes the
implemented slice from future event subscriptions, cards, HTTP ingress and
remote MCP.

## Executed checks

| Command / boundary                                                                                                    | Result                                                                                                                                                   |
| --------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo test -p mesh-talk-integrations -p mesh-talk-mcp`                                                               | 39 tests passed: service/private IPC/grants 25; operational CLI 7; MCP schemas/lifecycles 6; real MCP-to-core send 1.                                    |
| `cargo test -p mesh-talk-core --lib outgoing_ --features fast-test-kdf`                                               | 6 passed, including durable/restarted/concurrent retry, revocation, group sender recovery and bounded reply lookup.                                      |
| `cargo test -p mesh-talk-core --lib node::delivery_store::tests --features fast-test-kdf`                             | 36 passed, including capacity, crash/replay, frozen legacy bytes and retry-ledger limits.                                                                |
| `cargo test -p mesh-talk-core --lib participant_runtime_tests --features fast-test-kdf`                               | 5 passed, including graceful host shutdown joining nested children and admitted blocking work.                                                           |
| `cargo test -p mesh-talk-core --lib two_nodes_exchange_a_channel_message_over_loopback_tcp --features fast-test-kdf`  | 1 passed, with a bot-style alert and duplicate request over the existing encrypted group path.                                                           |
| `cargo test -p mesh-talk-core --lib two_nodes_transfer_a_file_in_a_channel --features fast-test-kdf`                  | 1 passed; covers sender-state changes shared with group file manifests.                                                                                  |
| `cargo test -p mesh-talk --lib integrations_`                                                                         | 6 passed: exact-session replacement/logout, lock after queued admission, manifest startup/cleanup and content-free acceptance notification emitted once. |
| `npm --prefix frontend test -- src/lib/events.test.ts src/store/chat.test.ts`                                         | 87 passed: payload forwarding, active matching chat refresh and stale/wrong-owner/account/event refusal, with existing store regressions.                |
| `cargo clippy -p mesh-talk-core -p mesh-talk-integrations -p mesh-talk-mcp -p mesh-talk --all-targets -- -D warnings` | Passed.                                                                                                                                                  |
| `cargo build -p mesh-talk-integrations -p mesh-talk-mcp`                                                              | Passed with normal production-strength KDF behavior, without the test feature.                                                                           |
| `npm --prefix frontend run typecheck`                                                                                 | Passed.                                                                                                                                                  |
| ESLint on changed types/events/chat store and tests                                                                   | Passed.                                                                                                                                                  |
| `npm --prefix frontend run build`                                                                                     | Passed; existing large-chunk advisory remains.                                                                                                           |
| Rust formatting, changed Markdown/frontend Prettier and `git diff --check`                                            | Passed.                                                                                                                                                  |

These commands select 181 distinct Rust/frontend tests in total. They do not
constitute the full workspace suite or a release acceptance score.

## Cross-boundary and binary evidence

The real-path MCP test negotiates current discovery through the official SDK,
passes a scoped call through actual private IPC, authorizes it in the shared
service, and appends a real encrypted group message. Same-key retry returns the
same message ID with one history entry. An ungranted destination, history tool
and description request are rejected. Separate SDK tests verify both
`2026-07-28` discovery and `2025-11-25` initialization.

A standalone normal-KDF binary smoke created a temporary dedicated bot profile,
issued a private scoped credential through the CLI, called the running host
through a real MCP stdio subprocess, and stopped the host with SIGTERM. Both the
admin file and endpoint manifest were removed on clean shutdown. This smoke
sent no peer messages; message-send assertions use isolated test groups and
loopback peers.

The standalone smoke exposed a default temporary-directory permission mismatch
that component fixtures had masked. A regression first reproduced mode `0755`;
the host now requests `0700` at directory creation, and the private-listener
regression and standalone smoke pass.

## Limits of this evidence

- Windows protected-file/pipe modules and their test sources were cross-compiled
  for `x86_64-pc-windows-msvc` in an isolated transport harness. Native Windows
  execution and a full Windows workspace build were not performed.
- No actual Feishu tenant, webhook or external AI provider was contacted.
- Desktop session and refresh tests use Tauri mocks and the frontend store; no
  physical desktop UI or Playwright process acceptance was performed here.
- The full workspace test suite, full health gate, backend E2E and cross-platform
  PR preflight were not run. Run the repository's required gates before publishing
  a PR or requesting review.
