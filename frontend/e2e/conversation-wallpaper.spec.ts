import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, seedThemeBeforeLoad } from "./helpers/session";

// GPU gradient compositing can round an individual color channel by one level
// between captures. Compare rendered pixels so this test still catches movement.
async function maxPixelDrift(
  page: import("@playwright/test").Page,
  a: Buffer,
  b: Buffer,
) {
  return page.evaluate(
    async ([first, second]) => {
      const pixels = async (base64: string) => {
        const image = await createImageBitmap(
          await (await fetch(`data:image/png;base64,${base64}`)).blob(),
        );
        const canvas = document.createElement("canvas");
        canvas.width = image.width;
        canvas.height = image.height;
        const context = canvas.getContext("2d");
        if (!context) throw new Error("Canvas unavailable");
        context.drawImage(image, 0, 0);
        const data = context.getImageData(0, 0, image.width, image.height).data;
        image.close();
        return data;
      };
      const left = await pixels(first);
      const right = await pixels(second);
      if (left.length !== right.length)
        throw new Error("Capture dimensions changed");
      let max = 0;
      for (let i = 0; i < left.length; i++)
        max = Math.max(max, Math.abs(left[i] - right[i]));
      return max;
    },
    [a.toString("base64"), b.toString("base64")],
  );
}

for (const theme of ["argentina", "barcelona", "messi"] as const) {
  test(`${theme} theme shows one image across the chat shell`, async ({
    page,
  }) => {
    await seedThemeBeforeLoad(page, theme);
    await enterChat(page);
    const homeBackground = await page
      .getByTestId("chat-shell")
      .evaluate((element) => getComputedStyle(element).backgroundImage);
    expect(homeBackground.includes("data:image/")).toBe(true);
    await openBobDm(page);

    const frame = page.locator("main.conversation-canvas");
    const frameBackground = await frame.evaluate(
      (element) => getComputedStyle(element).backgroundImage,
    );
    expect(frameBackground).toBe("none");
    const background = await page
      .getByTestId("chat-shell")
      .evaluate((element) => getComputedStyle(element).backgroundImage);
    expect(background.includes("data:image/")).toBe(true);
    expect(background).toContain("linear-gradient");
    await expect(page.locator(".conversation-log-surface")).toHaveCSS(
      "background-color",
      "rgba(0, 0, 0, 0)",
    );

    const sidebarBackground = await page
      .getByTestId("sidebar")
      .evaluate((element) => getComputedStyle(element).backgroundColor);
    expect(sidebarBackground).toMatch(/, 0\.74\)$/);
  });
}

test("base theme keeps a plain conversation canvas", async ({ page }) => {
  await enterChat(page);
  const homeBackground = await page
    .getByTestId("chat-shell")
    .evaluate((element) => getComputedStyle(element).backgroundImage);
  expect(homeBackground).toBe("none");
  await openBobDm(page);

  const background = await page
    .locator("main.conversation-canvas")
    .evaluate((element) => getComputedStyle(element).backgroundImage);
  expect(background).toBe("none");
});

test("wallpaper stays in place across conversation and composer changes", async ({
  page,
}) => {
  await seedThemeBeforeLoad(page, "barcelona");
  await enterChat(page);
  await openBobDm(page);
  const main = await page.locator("main").boundingBox();
  if (!main) throw new Error("Missing conversation surface");
  const clip = {
    x: main.x + 36,
    y: main.y + 150,
    width: 96,
    height: 96,
  };
  const privateWallpaper = await page.screenshot({ clip });

  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await expect(page.getByTestId("conversation-header")).toContainText("team");
  await expect(page.getByRole("log")).toBeVisible();
  const channelWallpaper = await page.screenshot({ clip });
  expect(
    await maxPixelDrift(page, channelWallpaper, privateWallpaper),
  ).toBeLessThanOrEqual(1);

  await page.getByTestId("composer-more-tools").click();
  await expect(page.getByRole("toolbar", { name: "Tools" })).toBeVisible();
  const expandedWallpaper = await page.screenshot({ clip });
  expect(
    await maxPixelDrift(page, expandedWallpaper, channelWallpaper),
  ).toBeLessThanOrEqual(1);
});

async function holdRememberedChat(
  page: import("@playwright/test").Page,
  runtimeReady = true,
  hold: "decode" | "library" | "selected" | null = null,
) {
  await seedThemeBeforeLoad(page, "barcelona");
  await page.addInitScript(
    ({ ready, hold }) => {
      let releaseAssets!: () => void;
      const assets = new Promise<void>((resolve) => {
        releaseAssets = resolve;
      });
      (
        window as unknown as { __releasePresentationAssets: () => void }
      ).__releasePresentationAssets = releaseAssets;
      if (hold === "decode") {
        const decode = HTMLImageElement.prototype.decode;
        HTMLImageElement.prototype.decode = async function () {
          await decode.call(this);
          (window as unknown as { __decodeWaiting: boolean }).__decodeWaiting =
            true;
          await assets;
        };
      } else if (hold === "library" || hold === "selected") {
        const getAll = IDBObjectStore.prototype.getAll;
        IDBObjectStore.prototype.getAll = function (...args) {
          const request = getAll.apply(this, args);
          if (
            this.transaction.db.name.startsWith(
              "mesh-talk-customization-account:",
            )
          ) {
            Object.defineProperty(request, "onsuccess", {
              set(handler) {
                request.addEventListener("success", (event) => {
                  void assets.then(() => handler.call(request, event));
                });
              },
            });
          }
          return request;
        };
      }
      if (hold === "selected") {
        const get = IDBObjectStore.prototype.get;
        IDBObjectStore.prototype.get = function (key) {
          const request = get.call(this, key);
          if (
            this.transaction.db.name.startsWith(
              "mesh-talk-customization-account:",
            )
          ) {
            (
              window as unknown as { __selectedThemeRequested: boolean }
            ).__selectedThemeRequested = true;
            Object.defineProperty(request, "onsuccess", {
              set(handler) {
                request.addEventListener("success", (event) => {
                  void assets.then(() => handler.call(request, event));
                });
              },
            });
          }
          return request;
        };
      }
      const native = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const invoke = native.invoke.bind(native);
      (window as unknown as { __presentationAcks: number }).__presentationAcks =
        0;
      native.invoke = async (command, args) => {
        if (command === "initial_presentation_ready") {
          (window as unknown as { __presentationAcks: number })
            .__presentationAcks++;
          return;
        }
        if (args?.owner === "u_other" && command === "owner_node_identity")
          throw new Error("Replacement account is still opening");
        if (args?.owner === "u_other" && command === "owner_startup_identity")
          return {
            owner: args.owner,
            startup_id: "replacement-pending",
            phase: "pending",
          };
        if (command === "auto_login")
          return { id: "u_self", username: "tester", display_name: "tester" };
        if (!ready && command === "owner_node_identity")
          throw new Error("Runtime is still opening");
        if (!ready && command === "owner_startup_identity")
          return {
            owner: args.owner,
            startup_id: "startup-wallpaper-fixture",
            phase: "identity",
            account_id: "acc_self_aaaa1111",
            device_id: "device_self_0001",
          };
        return invoke(command, args);
      };
    },
    { ready: runtimeReady, hold },
  );
  let releaseChat!: () => void;
  const held = new Promise<void>((resolve) => {
    releaseChat = resolve;
  });
  let requestedChat!: () => void;
  const requested = new Promise<void>((resolve) => {
    requestedChat = resolve;
  });
  await page.route("**/src/features/chat/ChatApp.tsx*", async (route) => {
    requestedChat();
    await held;
    await route.continue();
  });
  await page.goto("/");
  await requested;
  await expect(page.getByTestId("chat-shell")).toHaveCount(0);
  if (hold === "selected") {
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __selectedThemeRequested: boolean })
              .__selectedThemeRequested,
        ),
      )
      .toBe(true);
  } else {
    await expect(page.locator("[data-appearance-canvas]")).toHaveCSS(
      "background-image",
      /data:image\//,
    );
  }
  return releaseChat;
}

for (const runtimeReady of [true, false]) {
  test(`remembered account restores wallpaper before the chat module loads (${runtimeReady ? "ready" : "runtime blocked"})`, async ({
    page,
  }) => {
    const release = await holdRememberedChat(page, runtimeReady);
    release();
    await expect(page.getByTestId("chat-shell")).toBeVisible();
    await expect(page.getByTestId("chat-shell")).toHaveCSS(
      "background-image",
      /data:image\//,
    );
    await page.reload();
    await expect(page.getByTestId("chat-shell")).toHaveCSS(
      "background-image",
      /data:image\//,
    );
  });
}

for (const retirement of ["lock", "logout", "replacement"] as const) {
  test(`startup wallpaper is concealed immediately on ${retirement}`, async ({
    page,
  }) => {
    const release = await holdRememberedChat(page);
    try {
      await page.evaluate(async (action) => {
        const observation = window as unknown as {
          __retiredWallpaperFrames: boolean[];
        };
        observation.__retiredWallpaperFrames = [];
        const observer = new MutationObserver(() => {
          observation.__retiredWallpaperFrames.push(
            [
              ...document.querySelectorAll(
                "[data-appearance-canvas], [data-testid=chat-shell]",
              ),
            ].some((element) =>
              getComputedStyle(element).backgroundImage.includes("data:image/"),
            ),
          );
        });
        // Import before observing so module fetches cannot add samples while
        // the originating account is still legitimately visible.
        if (action === "lock") {
          const path = "/src/store/appLock.ts";
          const { useAppLock } = await import(/* @vite-ignore */ path);
          observer.observe(document.getElementById("root")!, {
            subtree: true,
            childList: true,
            attributes: true,
          });
          await useAppLock.getState().lock();
          await useAppLock.getState().unlock("incorrect");
        } else {
          const path = "/src/store/auth.ts";
          const { useAuth } = await import(/* @vite-ignore */ path);
          observer.observe(document.getElementById("root")!, {
            subtree: true,
            childList: true,
            attributes: true,
          });
          if (action === "logout") await useAuth.getState().logout();
          else
            useAuth.setState((state: { generation: number }) => ({
              user: { id: "u_other", username: "other", display_name: "other" },
              generation: state.generation + 1,
            }));
        }
      }, retirement);
      await expect(page.locator("[data-appearance-ready]")).toHaveCount(0);
      await expect
        .poll(() =>
          page.evaluate(() =>
            [
              ...document.querySelectorAll(
                "[data-appearance-canvas], [data-testid=chat-shell]",
              ),
            ].every(
              (element) =>
                !getComputedStyle(element).backgroundImage.includes(
                  "data:image/",
                ),
            ),
          ),
        )
        .toBe(true);
      expect(
        await page.evaluate(
          () =>
            (window as unknown as { __retiredWallpaperFrames: boolean[] })
              .__retiredWallpaperFrames,
        ),
      ).not.toContain(true);
      if (retirement === "lock") {
        await expect(page.getByTestId("lock-screen")).toBeVisible();
        await expect(page.getByRole("alert")).toContainText("Invalid password");
      } else if (retirement === "logout") {
        await expect(page.getByTestId("login-form")).toBeVisible();
      }
    } finally {
      release();
    }
  });
}

for (const startup of ["signed out", "locked", "resume rejected"] as const) {
  test(`private startup wallpaper stays concealed when ${startup}`, async ({
    page,
  }) => {
    await seedThemeBeforeLoad(page, "barcelona");
    await page.addInitScript((state) => {
      const native = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const invoke = native.invoke.bind(native);
      native.invoke = async (command, args) => {
        if (command === "lock_status" && state === "locked")
          return { id: "u_self", username: "tester", display_name: "tester" };
        if (command === "auto_login" && state === "resume rejected")
          throw new Error("Saved credentials rejected");
        return invoke(command, args);
      };
    }, startup);
    await page.goto("/");
    await expect(
      page.getByTestId(startup === "locked" ? "lock-screen" : "login-form"),
    ).toBeVisible();
    await expect(page.locator("[data-appearance-ready]")).toHaveCount(0);
    expect(
      await page.evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue(
          "--pack-wallpaper-url",
        ),
      ),
    ).toBe("");
  });
}

test("initial native presentation is acknowledged before lazy chat and full runtime readiness", async ({
  page,
}) => {
  const release = await holdRememberedChat(page, false);
  try {
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (window as unknown as { __presentationAcks: number })
                .__presentationAcks,
          ),
        { timeout: 2000 },
      )
      .toBe(1);
  } finally {
    release();
  }
});

for (const hold of ["decode"] as const) {
  test(`initial native presentation waits for selected wallpaper ${hold}`, async ({
    page,
  }) => {
    const release = await holdRememberedChat(page, false, hold);
    try {
      expect(
        await page.evaluate(
          () =>
            (window as unknown as { __presentationAcks: number })
              .__presentationAcks,
        ),
      ).toBe(0);
      await page.evaluate(() =>
        (
          window as unknown as { __releasePresentationAssets: () => void }
        ).__releasePresentationAssets(),
      );
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as unknown as { __presentationAcks: number })
                .__presentationAcks,
          ),
        )
        .toBe(1);
    } finally {
      release();
    }
  });
}

test("initial native presentation does not wait for unrelated library enumeration", async ({
  page,
}) => {
  const release = await holdRememberedChat(page, false, "library");
  try {
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (window as unknown as { __presentationAcks: number })
                .__presentationAcks,
          ),
        { timeout: 2000 },
      )
      .toBe(1);
  } finally {
    await page.evaluate(() =>
      (
        window as unknown as { __releasePresentationAssets: () => void }
      ).__releasePresentationAssets(),
    );
    release();
  }
});

test("initial native presentation waits for the selected theme lookup", async ({
  page,
}) => {
  const release = await holdRememberedChat(page, false, "selected");
  try {
    expect(
      await page.evaluate(
        () =>
          (window as unknown as { __presentationAcks: number })
            .__presentationAcks,
      ),
    ).toBe(0);
    await page.evaluate(() =>
      (
        window as unknown as { __releasePresentationAssets: () => void }
      ).__releasePresentationAssets(),
    );
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __presentationAcks: number })
              .__presentationAcks,
        ),
      )
      .toBe(1);
  } finally {
    release();
  }
});

for (const replacement of ["selection", "scope"] as const) {
  test(`pending wallpaper decode cannot acknowledge a retired ${replacement}`, async ({
    page,
  }) => {
    const release = await holdRememberedChat(page, false, "decode");
    try {
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as unknown as { __decodeWaiting: boolean })
                .__decodeWaiting,
          ),
        )
        .toBe(true);
      await page.evaluate(async (replacement) => {
        if (replacement === "scope") {
          const path = "/src/store/auth.ts";
          const { useAuth } = await import(/* @vite-ignore */ path);
          useAuth.setState((state: { generation: number }) => ({
            user: { id: "u_other", username: "other", display_name: "other" },
            generation: state.generation + 1,
          }));
        } else {
          const path = "/src/lib/theme.ts";
          const { useTheme } = await import(/* @vite-ignore */ path);
          useTheme.getState().set("dark");
        }
      }, replacement);
      if (replacement === "selection") {
        // The replacement plain selection needs no private image decoding.
        await expect
          .poll(() =>
            page.evaluate(
              () =>
                (window as unknown as { __presentationAcks: number })
                  .__presentationAcks,
            ),
          )
          .toBe(1);
      }
      await page.evaluate(async () => {
        (
          window as unknown as { __releasePresentationAssets: () => void }
        ).__releasePresentationAssets();
        await new Promise((resolve) => setTimeout(resolve, 0));
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      expect(
        await page.evaluate(
          () =>
            (window as unknown as { __presentationAcks: number })
              .__presentationAcks,
        ),
      ).toBe(replacement === "scope" ? 0 : 1);
    } finally {
      release();
    }
  });
}
