import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

test.use({ viewport: { width: 760, height: 520 } });

async function openAbout(page: import("@playwright/test").Page) {
  const action = page.getByTestId("sidebar-action-about");
  if (!(await action.isVisible()))
    await page.getByTestId("sidebar-overflow").click();
  await action.click();
  await expect(page.getByTestId("update-panel")).toBeVisible();
  await expectDialogFitsViewport(page, "about-dialog");
}

test("About checks a stable release and guides a verified manual install", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = internals.invoke;
    internals.invoke = async (cmd, args) => {
      if (cmd === "check_for_update")
        return {
          current_version: "0.3.2",
          latest_version: "0.4.0",
          is_newer: true,
          release_url:
            "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.4.0",
          notes: "Faster transfers and clearer status",
          package_name: "mesh-talk_v0.4.0_linux_x86_64.zip",
          package_size: 10485760,
          edition: "default",
        };
      if (cmd === "download_update") {
        (
          window as unknown as { __requestedUpdate?: string }
        ).__requestedUpdate = String(args.version);
        return {
          path: "/home/tester/Downloads/mesh-talk_v0.4.0_linux_x86_64.zip",
          file_name: "mesh-talk_v0.4.0_linux_x86_64.zip",
        };
      }
      return original(cmd, args);
    };
  });
  await openAbout(page);
  await expect(page.getByText("Version 0.4.0 is available")).toHaveCount(0);
  await page.getByTestId("check-for-updates").click();
  await expect(page.getByText("Version 0.4.0 is available")).toBeVisible();
  await expect(
    page.getByText("Faster transfers and clearer status"),
  ).toBeVisible();
  await page.getByTestId("download-update").click();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __requestedUpdate?: string }).__requestedUpdate,
    ),
  ).toBe("0.4.0");
  await expect(page.getByTestId("update-downloaded")).toContainText(
    "Download verified",
  );
  await expect(page.getByTestId("update-downloaded")).toContainText(
    "DEB or RPM",
  );
  await page.getByRole("button", { name: "Show in Downloads" }).click();
  await expect(page.getByTestId("update-error")).toHaveCount(0);
});

test("About reports an up-to-date app and a retryable check error", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = internals.invoke;
    let checks = 0;
    internals.invoke = async (cmd, args) => {
      if (cmd === "check_for_update") {
        if (checks++ === 0) throw new Error("Network unavailable");
        return {
          current_version: "0.3.2",
          latest_version: "0.3.0",
          is_newer: false,
          release_url:
            "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.3.0",
          notes: "",
          package_name: null,
          package_size: null,
          edition: "default",
        };
      }
      return original(cmd, args);
    };
  });
  await openAbout(page);
  await page.getByTestId("check-for-updates").click();
  await expect(page.getByTestId("update-error")).toContainText(
    "Network unavailable",
  );
  await page.getByTestId("check-for-updates").click();
  await expect(page.getByText("You're up to date (0.3.2).")).toBeVisible();
  await expect(page.getByTestId("update-error")).toHaveCount(0);
});

test("a long update download can be canceled and retried", async ({ page }) => {
  await enterChat(page);
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = internals.invoke;
    let rejectDownload: ((error: Error) => void) | null = null;
    internals.invoke = async (cmd, args) => {
      if (cmd === "check_for_update")
        return {
          current_version: "0.3.2",
          latest_version: "0.4.0",
          is_newer: true,
          release_url:
            "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.4.0",
          notes: "",
          package_name: "mesh-talk_v0.4.0_linux_x86_64.zip",
          package_size: 10485760,
          edition: "default",
        };
      if (cmd === "download_update")
        return new Promise((_, reject) => {
          rejectDownload = reject;
        });
      if (cmd === "cancel_update_download") {
        rejectDownload?.(new Error("Download canceled"));
        return null;
      }
      return original(cmd, args);
    };
  });
  await openAbout(page);
  await page.getByTestId("check-for-updates").click();
  await page.getByTestId("download-update").click();
  await expect(page.getByText("Downloading and verifying")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("about-dialog")).toHaveCount(0);
  await openAbout(page);
  await expect(page.getByText("Downloading and verifying")).toBeVisible();
  await page.getByRole("button", { name: "Cancel download" }).click();
  await expect(page.getByTestId("download-update")).toBeEnabled();
  await expect(page.getByTestId("update-error")).toHaveCount(0);
});
