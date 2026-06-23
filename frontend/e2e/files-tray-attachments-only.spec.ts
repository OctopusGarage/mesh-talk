import { test, expect } from "./tauri-mock";

// "Received files" is for ATTACHMENTS only, decided by the sender's INTENT (which button),
// NOT the file extension: a file sent via the media button is shown inline + auto-saved and
// never appears here; a file sent via the attach button DOES appear — even a .mov/.png.
const BOB_DEVICE = "device_bob_2222";
const BOB_ACCOUNT = "acc_bob_bbbb2222";
test.use({ viewport: { width: 1100, height: 800 } });

test("received-files tray lists attachments by intent, not extension", async ({
  page,
}) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  await page.evaluate(
    ({ dev, acct }) => {
      const emit = (
        window as unknown as { __mockEmit: (e: string, p: unknown) => void }
      ).__mockEmit;
      // Sent via the MEDIA button (media: true) → inline, excluded from the tray.
      emit("file-received", {
        from: dev,
        name: "holiday.png",
        size: 1234,
        file_conv: "fc_img_1",
        conv: acct,
        mime: "image/png",
        media: true,
      });
      // A pdf sent via the ATTACH button → in the tray.
      emit("file-received", {
        from: dev,
        name: "report.pdf",
        size: 5678,
        file_conv: "fc_pdf_1",
        conv: acct,
        mime: "application/pdf",
        media: false,
      });
      // A VIDEO sent via the ATTACH button → in the tray (extension would have hidden it).
      emit("file-received", {
        from: dev,
        name: "clip.mov",
        size: 9012,
        file_conv: "fc_mov_1",
        conv: acct,
        mime: "video/quicktime",
        media: false,
      });
    },
    { dev: BOB_DEVICE, acct: BOB_ACCOUNT },
  );

  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(tray).toBeVisible();
  await expect(tray.getByText("report.pdf")).toBeVisible(); // attachment shows
  await expect(tray.getByText("clip.mov")).toBeVisible(); // .mov attachment shows (by intent)
  await expect(tray.getByText("holiday.png")).toHaveCount(0); // media excluded
});
