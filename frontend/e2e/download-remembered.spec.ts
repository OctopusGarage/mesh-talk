import { test, expect } from "./tauri-mock";

// Regression: a downloaded attachment is remembered across reload — the row shows Reveal
// (its saved location), not Save, so a re-save never hits the pruned chunks ("file
// incomplete" error). Persisted in localStorage by file_conv.
const BOB_DEVICE = "device_bob_2222";
const BOB_ACCOUNT = "acc_bob_bbbb2222";
const PDF = {
  from: BOB_DEVICE,
  name: "report.pdf",
  size: 4242,
  file_conv: "fc_pdf_keep",
  conv: BOB_ACCOUNT,
  mime: "application/pdf",
};

async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
}

test("a downloaded attachment is remembered across reload", async ({
  page,
}) => {
  await login(page);
  await page.evaluate(
    (f) =>
      (
        window as unknown as { __mockEmit: (e: string, p: unknown) => void }
      ).__mockEmit("file-received", f),
    PDF,
  );
  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(tray.getByText("report.pdf")).toBeVisible();
  // Save it (download_dir is set → saveFileToDir, no dialog).
  await tray.getByRole("button", { name: "Save", exact: true }).click();
  await expect(tray.getByRole("button", { name: "Reveal" })).toBeVisible();

  // Reload + re-login + the same file arrives again → it must be REMEMBERED (Reveal), not Save.
  await page.reload();
  await login(page);
  await page.evaluate(
    (f) =>
      (
        window as unknown as { __mockEmit: (e: string, p: unknown) => void }
      ).__mockEmit("file-received", f),
    PDF,
  );
  await page.getByTestId("sidebar-action-files").click();
  const tray2 = page.getByTestId("files-tray");
  await expect(tray2.getByText("report.pdf")).toBeVisible();
  await expect(tray2.getByRole("button", { name: "Reveal" })).toBeVisible();
  await expect(
    tray2.getByRole("button", { name: "Save", exact: true }),
  ).toHaveCount(0);
});
