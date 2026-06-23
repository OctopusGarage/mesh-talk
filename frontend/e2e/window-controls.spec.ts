import { test, expect } from "./tauri-mock";

// Frameless window (Windows/Linux): the native title bar is dropped, so the app draws its
// own min/max/close. Force a non-Mac UA so needsCustomWindowControls() is true here (the CI
// runner is macOS, where they're intentionally hidden in favor of the native traffic-lights).
test.use({
  userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chromium/120",
});

test("frameless window shows custom min/max/close controls", async ({
  page,
}) => {
  await page.goto("/");
  for (const ctl of ["Minimize", "Maximize", "Close"]) {
    await expect(page.getByRole("button", { name: ctl })).toBeVisible();
  }
});
