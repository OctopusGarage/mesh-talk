# Visual Regression Matrix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add committed screenshot baselines for the core Mesh-Talk UI surfaces so theme, layout, and dialog regressions are caught automatically and preserved as reviewable diff artifacts.

**Architecture:** Reuse the existing Playwright + Tauri mock harness and layer `toHaveScreenshot` assertions on top of the current interaction/a11y E2E suite. Keep the snapshot scope small and stable: one shell matrix, one composer/dialog matrix, and one replacement for the ad-hoc members screenshot, with CI retaining both the HTML report and the pixel diff output when a visual check fails.

**Tech Stack:** Playwright screenshot assertions, existing `frontend/e2e` fixtures, GitHub Actions artifacts, and the current Vite-based frontend test environment.

---

### Task 1: Replace the ad-hoc members screenshot with a committed baseline

**Files:**
- Modify: `frontend/e2e/members-self.spec.ts`
- Create: `frontend/e2e/helpers/visual-snapshot.ts`
- Modify: `frontend/e2e/tauri-mock.ts`

- [ ] **Step 1: Add a reusable screenshot prep helper**

Create a helper like:

```ts
export async function prepareForScreenshot(page: Page) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.addStyleTag({
    content: `
      * { caret-color: transparent !important; }
      html, body { scroll-behavior: auto !important; }
    `,
  });
  await page.evaluate(async () => {
    // Wait for web fonts so text width is stable before snapshotting.
    await (document as Document & { fonts?: FontFaceSet }).fonts?.ready;
  });
}
```

Use it in the members dialog test before `toHaveScreenshot` so the dialog is stable across runs.

- [ ] **Step 2: Replace `page.screenshot({ path: "e2e/__screens__/members-self.png" })`**

Rewrite the spec so it asserts the dialog against a committed baseline instead of writing a throwaway PNG to `frontend/e2e/__screens__/`.

Expected shape:

```ts
await prepareForScreenshot(page);
await expect(dialog).toHaveScreenshot("members-self.png");
```

- [ ] **Step 3: Run the focused spec and generate the first baseline**

Run:

```bash
cd frontend && npx playwright test e2e/members-self.spec.ts --update-snapshots
```

Expected: Playwright writes a committed baseline under the spec snapshot folder and the test passes.

- [ ] **Step 4: Re-run the same spec without updates**

Run:

```bash
cd frontend && npx playwright test e2e/members-self.spec.ts
```

Expected: the screenshot assertion passes against the committed baseline.

### Task 2: Add a shell screenshot matrix across themes and viewport sizes

**Files:**
- Create: `frontend/e2e/ui-visual-regression.spec.ts`
- Modify: `frontend/e2e/helpers/session.ts`
- Modify: `frontend/e2e/helpers/visual-snapshot.ts`

- [ ] **Step 1: Add the shell matrix test cases**

Add committed screenshot assertions for the `chat-shell` region after logging in and opening Bob's DM. Cover these combinations:
- Themes: `light`, `dark`, `oled`, `argentina`, `barcelona`, `messi`
- Viewports: `1280x800` and `760x620`

Use a helper that applies a theme, opens the seeded DM, then snapshots the shell container only.

- [ ] **Step 2: Generate the missing baselines**

Run:

```bash
cd frontend && npx playwright test e2e/ui-visual-regression.spec.ts -g shell --update-snapshots
```

Expected: Playwright creates committed baseline PNGs for each theme/viewport combination.

- [ ] **Step 3: Verify the matrix without updates**

Run:

```bash
cd frontend && npx playwright test e2e/ui-visual-regression.spec.ts -g shell
```

Expected: all shell screenshots pass, with no diff output.

- [ ] **Step 4: Keep the matrix tight**

If any theme/viewport pair proves noisy, remove only that pair from the matrix rather than loosening the screenshot threshold. The matrix should remain small enough to run on every UI change.

### Task 3: Add screenshot coverage for composer and dialog surfaces

**Files:**
- Modify: `frontend/e2e/ui-audit-composer.spec.ts`
- Modify: `frontend/e2e/ui-audit-dialogs.spec.ts`
- Modify: `frontend/src/features/chat/Composer.tsx`
- Modify: `frontend/src/components/ui/dialog.tsx`

- [ ] **Step 1: Add committed screenshots for composer surfaces**

Add snapshot assertions for:
- the composer toolbar and input row at `1280x800`
- the emoji, sticker, and screenshot popovers at `760x620`

Use the existing seeded chat state so the snapshot represents the real UI after login and DM selection.

- [ ] **Step 2: Add committed screenshots for dialog surfaces**

Add snapshot assertions for:
- search dialog
- settings dialog
- profile dialog
- files tray / files dialog

Use the same `820x620` viewport used by the current audit tests so the visual baseline stays aligned with the interaction checks.

- [ ] **Step 3: Generate and verify the baselines**

Run:

```bash
cd frontend && npx playwright test e2e/ui-audit-composer.spec.ts e2e/ui-audit-dialogs.spec.ts --update-snapshots
```

Then rerun:

```bash
cd frontend && npx playwright test e2e/ui-audit-composer.spec.ts e2e/ui-audit-dialogs.spec.ts
```

Expected: both spec files pass with committed baselines.

- [ ] **Step 4: Keep the UI affordance fixes**

Do not relax the existing focus/size/layout checks. The screenshot layer is additive; the size and focus assertions remain the guardrails for interaction quality.

### Task 4: Preserve visual diff evidence in CI and remove the obsolete screenshot path

**Files:**
- Modify: `.github/workflows/e2e-ui.yml`
- Modify: `.gitignore`
- Modify: `frontend/.gitignore`

- [ ] **Step 1: Upload visual diff artifacts on failure**

Extend the existing failure artifact upload so CI preserves:
- `frontend/playwright-report/`
- `frontend/test-results/`

This keeps the PNG diff output and trace data available when a visual baseline fails.

- [ ] **Step 2: Remove the obsolete ad-hoc screenshot ignore entry**

Delete the `frontend/e2e/__screens__/` ignore rule once `members-self.spec.ts` no longer writes there.

- [ ] **Step 3: Run the full frontend UI suite**

Run:

```bash
cd frontend && npm run e2e
```

Expected: the full Playwright suite passes with the new visual matrix included.

- [ ] **Step 4: Re-run the frontend quality gates**

Run:

```bash
cd frontend && npm run test && npm run typecheck && npm run lint
```

Expected: no regressions outside the screenshot layer.

---

### Review Notes

This plan intentionally stops at a small, high-signal screenshot matrix. The commit history already shows the core interaction/a11y risks are covered; this layer is meant to catch pixel-level regressions in the shell, composer, and modal surfaces, not to snapshot every screen in the app. If a snapshot becomes flaky, fix the UI stabilization or narrow the matrix before considering any threshold relaxation.
