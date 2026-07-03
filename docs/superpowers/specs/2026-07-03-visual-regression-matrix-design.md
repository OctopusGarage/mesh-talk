# Visual Regression Matrix Design

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add committed visual regression coverage for the most important Mesh-Talk UI surfaces so theme, layout, and dialog regressions are caught by screenshot diffs instead of by hand.

**Architecture:** Keep the existing DOM/a11y E2E checks in place and add a separate Playwright screenshot layer on top. The screenshot layer will use committed baselines for stable regions of the app, exercised across a small but meaningful theme/viewport matrix, with failure artifacts uploaded from CI so diffs are reviewable.

**Tech Stack:** Playwright `toHaveScreenshot`, the existing Tauri mock harness, GitHub Actions artifacts, and the current `frontend/e2e` test layout.

---

### Task 1: Create a stable visual snapshot harness

**Files:**
- Create: `frontend/e2e/helpers/visual-snapshot.ts`
- Modify: `frontend/e2e/tauri-mock.ts`
- Modify: `frontend/e2e/members-self.spec.ts`

- [ ] **Step 1: Define the failing visual helper**

Introduce a helper that prepares a page for screenshots by waiting for fonts, hiding the caret, and turning off animation noise before any `toHaveScreenshot` call.

- [ ] **Step 2: Reproduce the current ad-hoc screenshot usage**

Run: `cd frontend && npx playwright test e2e/members-self.spec.ts`
Expected: the spec still writes a one-off image to `e2e/__screens__/members-self.png`, which is not a real regression baseline.

- [ ] **Step 3: Replace the ad-hoc capture with a baseline assertion**

Update the spec so it asserts the dialog against a committed screenshot baseline instead of writing a file under `__screens__`.

- [ ] **Step 4: Run the focused visual spec**

Run: `cd frontend && npx playwright test e2e/members-self.spec.ts --update-snapshots`
Expected: Playwright creates or updates a committed baseline snapshot for the dialog.

### Task 2: Add a theme and viewport screenshot matrix for the shell

**Files:**
- Create: `frontend/e2e/ui-visual-regression.spec.ts`
- Modify: `frontend/e2e/helpers/session.ts`

- [ ] **Step 1: Write the failing shell screenshot cases**

Add screenshot assertions for the main shell container at these matrix points:
- Themes: `light`, `dark`, `oled`, `argentina`, `barcelona`, `messi`
- Viewports: `1280x800` and `760x620`

Capture the `chat-shell` region after entering the chat and opening Bob's DM, so the same layout is exercised under the same seeded conversation state.

- [ ] **Step 2: Run the shell matrix once without baselines**

Run: `cd frontend && npx playwright test e2e/ui-visual-regression.spec.ts -g shell`
Expected: Playwright reports missing snapshots and writes actual/diff artifacts for the new matrix.

- [ ] **Step 3: Add the committed baselines**

Use Playwright snapshot updates to generate the baseline images for the shell matrix.

- [ ] **Step 4: Re-run the shell matrix**

Run: `cd frontend && npx playwright test e2e/ui-visual-regression.spec.ts -g shell`
Expected: all shell screenshots pass against the committed baselines.

### Task 3: Add screenshot coverage for composer and dialog surfaces

**Files:**
- Modify: `frontend/e2e/ui-audit-composer.spec.ts`
- Modify: `frontend/e2e/ui-audit-dialogs.spec.ts`
- Modify: `frontend/src/features/chat/Composer.tsx`
- Modify: `frontend/src/components/ui/dialog.tsx`

- [ ] **Step 1: Write the failing composer and dialog screenshots**

Add committed screenshot checks for:
- the composer toolbar and input row at `1280x800`
- the emoji/sticker/screenshot popovers at `760x620`
- the search, settings, profile, and files dialogs at `820x620`

Use the same seeded chat state as the existing audit tests so the visual assertions sit on top of already-stable interactions.

- [ ] **Step 2: Run the targeted spec files once without baselines**

Run: `cd frontend && npx playwright test e2e/ui-audit-composer.spec.ts e2e/ui-audit-dialogs.spec.ts`
Expected: missing snapshot failures for the new screenshot assertions.

- [ ] **Step 3: Commit the baseline screenshots**

Update the snapshots so the committed images become the visual contract for these surfaces.

- [ ] **Step 4: Re-run the targeted specs**

Run: `cd frontend && npx playwright test e2e/ui-audit-composer.spec.ts e2e/ui-audit-dialogs.spec.ts`
Expected: all screenshot assertions pass.

### Task 4: Make CI preserve visual diff evidence

**Files:**
- Modify: `.github/workflows/e2e-ui.yml`
- Modify: `.gitignore`
- Modify: `frontend/.gitignore`

- [ ] **Step 1: Add artifact upload for screenshot diffs**

Upload `frontend/test-results/` on failure in addition to `frontend/playwright-report/` so reviewable diff images are preserved when a visual regression fails.

- [ ] **Step 2: Remove the obsolete ad-hoc screenshot path**

Delete the `frontend/e2e/__screens__/` ignore entry after the last one-off screenshot is replaced by a committed baseline.

- [ ] **Step 3: Run the full UI suite**

Run: `cd frontend && npm run e2e`
Expected: the existing UI E2E suite passes with the new visual baseline tests included.

- [ ] **Step 4: Run the workflow-equivalent checks locally**

Run: `cd frontend && npm run test && npm run typecheck && npm run lint`
Expected: no regressions in the supporting frontend checks.

---

### Review Notes

This design intentionally keeps the visual scope tight: it covers the shell, composer, and modal/dialog surfaces that are most likely to regress from theme or layout changes, and it reuses the existing stable E2E harness instead of introducing a new screenshot toolchain. If the first pass proves too noisy, the fallback is to narrow the matrix before adding more surfaces, not to relax the screenshot assertions themselves.
