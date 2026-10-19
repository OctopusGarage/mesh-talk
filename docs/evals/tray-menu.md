# Tray menu validation — 2026-10-16

## Scope

The native menu provides Open Mesh-Talk, an unread summary, Connection Diagnostics,
Settings, About, Hide, and Quit. The source review covers menu construction,
window activation, unread updates, and frontend navigation. Browser tests exercise
the real React listeners through mocked Tauri events; they do not render or click
the native menu bar.

## Regression coverage

`frontend/e2e/tray-navigation.spec.ts` covers opening and dismissing each utility
dialog without first opening the sidebar menu, ignoring unknown destinations,
replacing an existing utility dialog, and retaining the sidebar About action.

These checks reproduced two defects before their fixes:

- About's listener was inside unmounted popover content. The utility component
  now owns its state and always mounts the controlled About dialog.
- Switching tray destinations left the previous dialog underneath the new one.
  Known destinations now close the other utility dialogs.

The Linux native geometry driver now activates the renamed **Open Mesh-Talk**
item. Its report tests validate the exported label and geometry evidence; a live
Linux desktop run is still required to verify the actual native interaction.

## Repeat the checks

```bash
cargo test -p mesh-talk --lib tray::tests
node --test scripts/diagnostics/tray-geometry-report.test.mjs
cd frontend
npm run e2e -- e2e/tray-navigation.spec.ts
```

From the repository root, run the source evaluation with a real model provider:

```bash
AI_EVAL_COMMAND='codex exec --sandbox read-only --ephemeral' \
  node scripts/ai-eval.mjs \
  --suite docs/evals/tray-menu-ai-eval-suite.json \
  --report target/ai-eval/tray-menu-report.json
```

The three cases review native menu behavior, frontend dialog ownership and
listener cleanup, and the controlled About interface. Model verdicts supplement
the regression tests. They do not prove native appearance, operating-system
focus behavior, or usability. Signed-out and locked navigation require a
separate native session check.

## Executed validation

- `make smoke-full`: passed. This includes the workspace Rust suite, 403 frontend
  unit tests, backend multi-process E2E, and 302 browser E2E checks. Two lite-only
  checks and the opt-in site screenshot capture were skipped in this default-edition run.
- The six added tray navigation browser tests: passed after reproducing and
  fixing the About and overlapping-dialog regressions.
- The four tray geometry report tests: passed.
- Frontend typecheck, lint, affected-source formatting, production build, and
  the eight sidebar unit tests: passed after the fixes.
- The tray-specific Codex source evaluation: all three cases passed, each scored
  1.0 with no findings or required improvements. The local machine-readable
  report is `target/ai-eval/tray-menu-report.json`.
- The standard project-guidance Codex evaluation: all ten cases passed with no
  findings or required improvements. Its report is `target/ai-eval/report.json`.

The first model-provider attempt used Claude CLI, whose configured GLM endpoint
rejected the request with HTTP 429 and an expired-plan message. Evaluation was
then run through Codex CLI in read-only mode.
