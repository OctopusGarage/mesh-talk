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
          channel: "stable",
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
          channel: "stable",
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
          channel: "stable",
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
  await expect(
    page.getByRole("combobox", { name: "Update channel" }),
  ).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("about-dialog")).toHaveCount(0);
  await openAbout(page);
  await expect(page.getByText("Downloading and verifying")).toBeVisible();
  await expect(
    page.getByRole("combobox", { name: "Update channel" }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "Cancel download" }).click();
  await expect(page.getByTestId("download-update")).toBeEnabled();
  await expect(page.getByTestId("update-error")).toHaveCount(0);
});

test("Nightly choice persists, discards candidates and downloads the same desktop package", async ({
  page,
}) => {
  await enterChat(page);
  await openAbout(page);
  const choice = page.getByRole("combobox", { name: "Update channel" });
  await expect(choice).toHaveValue("stable");
  await choice.selectOption("nightly");
  await expect(
    page.getByText(/Experimental builds may contain bugs/),
  ).toBeVisible();
  expect(
    await page.evaluate(() => localStorage.getItem("mesh-talk-update-channel")),
  ).toBe("nightly");
  await page.reload();
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
      if (cmd === "check_for_update") {
        (window as unknown as { __checkedChannel: unknown }).__checkedChannel =
          args.updateChannel;
        return {
          channel: args.updateChannel,
          current_version: "0.4.4-nightly.41.1",
          latest_version: "0.4.4-nightly.42.1",
          is_newer: true,
          release_url:
            "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.4.4-nightly.42.1",
          notes: "Nightly preview",
          package_name: "mesh-talk_v0.4.4-nightly.42.1_linux_x86_64.zip",
          package_size: 42,
          edition: "default",
        };
      }
      if (cmd === "download_update") {
        (
          window as unknown as { __downloadRequest: unknown }
        ).__downloadRequest = {
          version: args.version,
          channel: args.updateChannel,
        };
        return {
          path: "/Downloads/mesh-talk_v0.4.4-nightly.42.1_linux_x86_64.zip",
          file_name: "mesh-talk_v0.4.4-nightly.42.1_linux_x86_64.zip",
        };
      }
      return original(cmd, args);
    };
  });
  await openAbout(page);
  await expect(choice).toHaveValue("nightly");
  await expect(page.getByTestId("download-update")).toHaveCount(0);
  await page.getByTestId("check-for-updates").click();
  await expect(page.getByText("Installed: 0.4.4-nightly.41.1")).toBeVisible();
  await expect(
    page.getByText(/mesh-talk_v0.4.4-nightly.42.1_linux_x86_64.zip/),
  ).toBeVisible();
  await choice.selectOption("stable");
  await expect(page.getByTestId("download-update")).toHaveCount(0);
  await expect(page.getByText("Nightly preview")).toHaveCount(0);
  await choice.selectOption("nightly");
  await page.getByTestId("check-for-updates").click();
  await page.getByTestId("download-update").click();
  await expect(page.getByTestId("update-downloaded")).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __checkedChannel: unknown }).__checkedChannel,
    ),
  ).toBe("nightly");
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __downloadRequest: unknown }).__downloadRequest,
    ),
  ).toEqual({
    version: "0.4.4-nightly.42.1",
    channel: "nightly",
  });
});

test("a signed update offers one-click installation and retries authorization without downloading again", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const window_ = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __updatePreparations: number;
      __updateInstallations: number;
      __installedUpdate?: unknown;
    };
    const original = window_.__TAURI_INTERNALS__.invoke;
    window_.__updatePreparations = 0;
    window_.__updateInstallations = 0;
    window_.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "check_for_update")
        return {
          channel: "stable",
          automatic_install: true,
          current_version: "0.4.4",
          latest_version: "0.4.5",
          is_newer: true,
          release_url:
            "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.4.5",
          notes: "Smoother desktop updates",
          package_name: "mesh-talk-update_v0.4.5_macos_arm64.app.tar.gz",
          package_size: 42,
          edition: "default",
        };
      if (command === "prepare_update") {
        window_.__updatePreparations++;
        return {
          path: "/private/verified-update/payload",
          file_name: "app.tar.gz",
          installable: true,
        };
      }
      if (command === "install_update") {
        window_.__installedUpdate = {
          version: args.version,
          channel: args.updateChannel,
        };
        if (++window_.__updateInstallations === 1)
          throw new Error("Authorization canceled");
        return null;
      }
      return original(command, args);
    };
  });
  await openAbout(page);
  await page.getByTestId("check-for-updates").click();
  await page
    .getByRole("button", { name: "Update and restart", exact: true })
    .click();
  await expect(page.getByTestId("update-error")).toContainText(
    "Authorization canceled",
  );
  await expect(
    page.getByRole("button", { name: "Download manually", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Restart to update", exact: true })
    .click();
  await expect(page.getByTestId("update-install-status")).toContainText(
    "Restarting Mesh-Talk",
  );
  await expect(page.getByTestId("check-for-updates")).toBeDisabled();
  expect(
    await page.evaluate(() => {
      const result = window as unknown as {
        __updatePreparations: number;
        __updateInstallations: number;
        __installedUpdate: unknown;
      };
      return [
        result.__updatePreparations,
        result.__updateInstallations,
        result.__installedUpdate,
      ];
    }),
  ).toEqual([1, 2, { version: "0.4.5", channel: "stable" }]);
});

test("incremental preparation and full fallback show truthful progress and cancellation prevents installation", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __emitUpdateProgress?: (progress: {
        downloaded: number;
        total: number;
        phase?: "preparing" | "fallback";
      }) => void;
      __updateInstallations: number;
    };
    const original = host.__TAURI_INTERNALS__.invoke;
    let rejectPreparation: (reason: Error) => void = () => {};
    host.__updateInstallations = 0;
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "check_for_update")
        return {
          channel: "stable",
          automatic_install: true,
          current_version: "0.4.4",
          latest_version: "0.4.5",
          is_newer: true,
          release_url:
            "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.4.5",
          notes: "Incremental download",
          package_name: "mesh-talk-update_v0.4.5_macos_arm64.app.tar.gz",
          package_size: 10485760,
          edition: "default",
        };
      if (command === "prepare_update") {
        const channel = args.onProgress as {
          onmessage: NonNullable<typeof host.__emitUpdateProgress>;
        };
        host.__emitUpdateProgress = (progress) => channel.onmessage(progress);
        return new Promise((_resolve, reject) => {
          rejectPreparation = reject;
        });
      }
      if (command === "cancel_update_download") {
        rejectPreparation(new Error("Download canceled"));
        return null;
      }
      if (command === "install_update") {
        host.__updateInstallations++;
        return null;
      }
      return original(command, args);
    };
  });
  await openAbout(page);
  await page.getByTestId("check-for-updates").click();
  await page
    .getByRole("button", { name: "Update and restart", exact: true })
    .click();
  await page.evaluate(() =>
    (
      window as unknown as { __emitUpdateProgress: (value: unknown) => void }
    ).__emitUpdateProgress({ downloaded: 50, total: 100 }),
  );
  await expect(page.getByTestId("update-panel")).toContainText("50%");
  await page.evaluate(() =>
    (
      window as unknown as { __emitUpdateProgress: (value: unknown) => void }
    ).__emitUpdateProgress({ phase: "preparing", downloaded: 0, total: 0 }),
  );
  await expect(
    page.getByText("Preparing update…", { exact: true }),
  ).toBeVisible();
  await expect(page.locator("progress")).not.toHaveAttribute("value");
  await expect(
    page.getByLabel("Update channel", { exact: true }),
  ).toBeDisabled();
  await page.evaluate(() =>
    (
      window as unknown as { __emitUpdateProgress: (value: unknown) => void }
    ).__emitUpdateProgress({
      phase: "fallback",
      downloaded: 0,
      total: 10485760,
    }),
  );
  await expect(
    page.getByText("Downloading full update…", { exact: true }),
  ).toBeVisible();
  await expect(page.locator("progress")).toHaveAttribute("value", "0");
  await expectDialogFitsViewport(page, "about-dialog");
  await page
    .getByRole("button", { name: "Cancel download", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Update and restart", exact: true }),
  ).toBeEnabled();
  await expect(page.getByTestId("update-error")).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __updateInstallations: number })
          .__updateInstallations,
    ),
  ).toBe(0);
});
