import { waitForChatShell } from "./helpers/session";
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
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
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
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          localStorage.getItem("mesh-talk-downloads:tester") ??
          Object.keys(localStorage)
            .filter((key) => key.startsWith("mesh-talk-downloads:"))
            .map((key) => localStorage.getItem(key))
            .join(""),
      ),
    )
    .toContain("fc_pdf_keep");
  await page.getByTestId("sidebar-action-files").click();
  const tray = page.getByTestId("files-tray");
  await expect(tray.getByText("report.pdf")).toBeVisible();
  // The closed tray did not prevent automatic export to the download directory.
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

// Mocked IPC verifies UI recovery and the selected destination, not native disk writes.
for (const action of ["Retry", "Save as…"]) {
  test(`automatic export clears the failed write after ${action} succeeds`, async ({
    page,
  }) => {
    await login(page);
    await page.evaluate(
      (file) => {
        const win = window as unknown as {
          __saveAttempts: number;
          __saveDestinations: unknown[];
          __mockEmit: (event: string, payload: unknown) => void;
          __TAURI_INTERNALS__: {
            invoke: (
              cmd: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        };
        win.__saveAttempts = 0;
        win.__saveDestinations = [];
        const original = win.__TAURI_INTERNALS__.invoke;
        win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
          if (cmd === "save_file_to_dir") {
            win.__saveDestinations.push(args.dir);
            if (++win.__saveAttempts === 1) throw new Error("disk full");
          }
          if (cmd === "save_file") win.__saveDestinations.push(args.dest);
          if (cmd === "plugin:dialog|save") return "/home/tester/report.pdf";
          return original(cmd, args);
        };
        win.__mockEmit("file-received", file);
      },
      { ...PDF, file_conv: "fc_failed_write" },
    );
    await page.getByTestId("sidebar-action-files").click();
    const tray = page.getByTestId("files-tray");
    await expect(tray.getByRole("alert")).toContainText("disk full");
    await expect(
      tray.getByRole("button", { name: action, exact: true }),
    ).toBeEnabled();
    await tray.getByRole("button", { name: action, exact: true }).click();
    await expect(
      tray.getByRole("button", { name: "Reveal", exact: true }),
    ).toBeVisible();
    await expect(tray.getByRole("alert")).toHaveCount(0);
    const destinations = await page.evaluate(
      () =>
        (window as unknown as { __saveDestinations: string[] })
          .__saveDestinations,
    );
    expect(destinations).toEqual([
      "/home/tester/Downloads",
      action === "Retry" ? "/home/tester/Downloads" : "/home/tester/report.pdf",
    ]);
  });
}
