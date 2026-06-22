import { test, expect } from "./tauri-mock";

// "Received files" is for ATTACHMENTS only — images/videos are auto-saved + shown inline,
// so they must NOT appear in this tray.
const BOB_DEVICE = "device_bob_2222";
const BOB_ACCOUNT = "acc_bob_bbbb2222";
test.use({ viewport: { width: 1100, height: 800 } });

test("received-files tray lists attachments, not images", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  // A peer sends an image AND a pdf attachment.
  await page.evaluate(
    ({ dev, acct }) => {
      const emit = (
        window as unknown as { __mockEmit: (e: string, p: unknown) => void }
      ).__mockEmit;
      emit("file-received", {
        from: dev,
        name: "holiday.png",
        size: 1234,
        file_conv: "fc_img_1",
        conv: acct,
        mime: "image/png",
      });
      emit("file-received", {
        from: dev,
        name: "report.pdf",
        size: 5678,
        file_conv: "fc_pdf_1",
        conv: acct,
        mime: "application/pdf",
      });
    },
    { dev: BOB_DEVICE, acct: BOB_ACCOUNT },
  );

  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(tray).toBeVisible();
  await expect(tray.getByText("report.pdf")).toBeVisible(); // attachment shows
  await expect(tray.getByText("holiday.png")).toHaveCount(0); // image excluded
});
