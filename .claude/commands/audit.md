---
description: Audit selected Mesh Talk boundaries or run an explicitly requested comprehensive gate
argument-hint: "[quick|full]"
allowed-tools: Bash, Read
---

`quick` (default) reviews the changed boundary, runs affected unit/integration
tests and the relevant static checks. Follow `docs/testing-feedback-loop.md`;
do not automatically launch full Rust, browser, physical or coverage suites.
Use `nice -n 10` for heavy direct Cargo commands.

`full` explicitly requests `./scripts/check-health.sh`, the maintained review/CI
health entry point. Its source owns the scan/test/build list; do not maintain a
second partial copy here and call it equivalent. An explicit `--fast` health run
is partial and must be labeled as such.

Model-backed evals require a configured provider. Mutation, release and physical
acceptance workflows are separate triggers, not side effects of a quick audit.

Report the commands actually run, their results, concrete findings and omitted
boundaries. Never claim the full gate passed from a focused or partial run.
