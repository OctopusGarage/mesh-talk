# UI and UX AI Review — 2026-10-06

## Scope and evidence

This review used `gpt-6-sol` through Codex CLI 0.159.3 in read-only mode to inspect
four committed Playwright screenshots:
dark chat at 1280×800 and 760×620, dark Settings at 820×620, and light chat at
760×620. The model was asked for at most five observations grounded in visible
pixels and to mark findings that need interaction checks. Browser tests then
checked behavior; screenshots alone cannot prove a control is broken or usable.

The reusable review prompt is `docs/evals/prompts/ui-ux-review.md`. For a repeat
review, give a vision-capable model that prompt plus screenshots from
`frontend/e2e/*-snapshots/`, then verify each recommendation against the running
app and the source before changing it. Record the model, date, screenshots, and
test results with the findings.

The existing `make ai-eval` suite evaluates agent instructions, not the app UI.
For reproducible layout checks, run:

```bash
cd frontend
npm run e2e -- e2e/ui-audit-shell.spec.ts e2e/ui-audit-dialogs.spec.ts e2e/ui-visual-regression.spec.ts
```

A human should inspect updated screenshots at the tested sizes and themes.

## Findings and decisions

| Observation | Evidence | Action |
| --- | --- | --- |
| Search and Received files look like unexplained icons on the narrow chat shell. | Static screenshot; source confirmed icon-only buttons with hover titles. A browser check exposed label overflow in Spanish and Japanese. | Show visible labels, keep full accessible names, and fit actions in all six supported languages. |
| The LAN footer shows a dot and a bare number. | Screenshot and sidebar source. | Add a compact translated “online” label and full accessible description. |
| The Settings intro omits its first Privacy section. | Screenshot and source. | Name the actual sections in the intro. |
| Theme choices continue below the Settings viewport without a scroll cue. | Screenshot; browser audit confirmed the dialog fits and scrolls. | Add a visible control that scrolls toward the next settings. |
| Secondary metadata may be small in the light theme. | Static screenshot only; contrast tests already exist. | Recheck with users or a measured readability audit before changing the palette. |

The browser audit passed before changes. The work here addresses the first four
confirmed presentation issues. No usability study or native Tauri test was run;
those are separate evidence for real-world task success.

A follow-up review with the same model on the updated dark and light 760×620 chat
screens and dark 820×620 Settings screen found all four visual issues addressed
and no new visible regression. It noted that theme names still require scrolling
at 820×620; the new cue makes that continuation explicit. The Playwright scroll
interaction test verifies the cue moves the list.

## Validation

- `make ai-eval` with a real Codex CLI provider: 5/5 agent-contract cases passed.
- `./scripts/check-health.sh`: passed, including 119 browser E2E cases, 193 frontend
  unit tests, 709 core Rust unit tests, security checks, linting, and builds.
- Visual baselines were refreshed for all six themes at 1280×800 and 760×620;
  the dark and light narrow chat screens and dark Settings screen were inspected.
