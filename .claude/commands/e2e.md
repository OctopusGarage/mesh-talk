---
description: Run the end-to-end multi-process integration tests and report a triaged verdict
allowed-tools: Task
---

Dispatch the `e2e-runner` subagent to run Mesh-Talk's end-to-end integration tests (real
`mesh-talk-node` processes over UDP discovery + TCP: two-node DM, history-survives-restart,
post-office offline delivery) and return its verdict.

The subagent runs the slow suite in its own context so the ~5-minute test output stays out of
this conversation — you get back only the per-test ✅/❌ and, for any failure, the triaged cause
(test-rig bug vs flaky timeout vs **real product regression** vs environmental). Relay it here.
