---
description: Run explicitly requested backend process scenarios and report a triaged verdict
allowed-tools: Task
---

Dispatch the `e2e-runner` subagent with the user's requested scenario and scope.
It selects affected process tests by default; an explicit comprehensive E2E
request uses the root `make e2e` target. Do not duplicate its evolving test list
or assume production KDF cold-start timings for cheap-KDF test binaries.

Return the exact commands, observed results and feature/topology boundaries.
Failures must distinguish rig errors, environment/ACL limits, contention and
reproducible product regressions. Read-only triage does not authorize service
changes, release publication or a repository-wide optimization loop.
