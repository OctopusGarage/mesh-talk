import { waitForChatShell } from "./helpers/session";
import { selectSettingsCategory } from "./helpers/settings";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { test, expect } from "./tauri-mock";
import { seedCustomizationPacks, seedMarketPacks } from "./helpers/packs";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { strToU8, unzipSync, zipSync } from "fflate";
import { parsePack } from "../src/lib/pack";
test.use({ viewport: { width: 1100, height: 800 } });

async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
}
const palette = (page: import("@playwright/test").Page) =>
  page.evaluate(() => document.documentElement.getAttribute("data-palette"));

test("theme picker applies + persists a brand palette, and clears it for base modes", async ({
  page,
}) => {
  await seedMarketPacks(page, ["barcelona"]);
  await login(page);
  const onlineColor = () =>
    page
      .locator('[data-testid="lan-online-count"] [role="status"] > span')
      .last()
      .evaluate((dot) => getComputedStyle(dot).backgroundColor);
  const darkOnline = await onlineColor();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await expect(page.getByTestId("theme-picker")).toBeVisible();

  await page.getByTestId("theme-barcelona").click();
  await expect.poll(() => palette(page)).toBe("barcelona");
  expect(
    await page.evaluate(() => localStorage.getItem("mesh-talk-theme")),
  ).toBe("barcelona");
  // The brand crest surfaces in the footer mark while the theme is active.
  await expect(page.getByTestId("theme-crest")).toBeVisible();
  expect(await onlineColor()).toBe(darkOnline);

  await page.getByTestId("theme-dark").click();
  await expect.poll(() => palette(page)).toBe(null); // base mode clears the palette
  await expect(page.getByTestId("theme-crest")).toBeHidden(); // back to the app mark
});

test("theme changes stay scoped and reduced motion keeps control feedback", async ({
  page,
}) => {
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");

  const normal = await page.getByTestId("theme-light").evaluate((button) => {
    (button as HTMLButtonElement).click();
    const root = document.documentElement;
    const sidebar = document.querySelector(".sidebar-container");
    return {
      transitioning: root.classList.contains("theme-transitioning"),
      sidebarProperty: sidebar && getComputedStyle(sidebar).transitionProperty,
      injectedUniversalRule: [...document.head.querySelectorAll("style")].some(
        (style) =>
          style.textContent?.includes("*,*::before,*::after{transition:"),
      ),
    };
  });
  expect(normal).toEqual({
    transitioning: true,
    sidebarProperty: "background-color",
    injectedUniversalRule: false,
  });
  await expect
    .poll(() =>
      page.evaluate(() =>
        document.documentElement.classList.contains("theme-transitioning"),
      ),
    )
    .toBe(false);

  await page.emulateMedia({ reducedMotion: "reduce" });
  const reduced = await page.getByTestId("theme-dark").evaluate((button) => {
    (button as HTMLButtonElement).click();
    const style = getComputedStyle(button);
    return {
      transitioning: document.documentElement.classList.contains(
        "theme-transitioning",
      ),
      properties: style.transitionProperty,
      duration: style.transitionDuration,
    };
  });
  expect(reduced.transitioning).toBe(false);
  expect(reduced.properties).toContain("background-color");
  expect(reduced.properties).not.toContain("transform");
  expect(reduced.duration).toBe("0.08s");
});

test("theme previews match the canvas, signal, and rail colors they apply", async ({
  page,
}) => {
  await seedMarketPacks(page, [
    "argentina",
    "barcelona",
    "messi",
    "nature",
    "cat-acrylic",
  ]);
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  for (const id of [
    "dark",
    "light",
    "oled",
    "argentina",
    "barcelona",
    "messi",
    "nature",
    "cat-acrylic",
  ]) {
    const preview = page.getByTestId(`theme-preview-${id}`);
    const previewCanvas = await preview.evaluate(
      (element) => getComputedStyle(element).backgroundColor,
    );
    await page.getByTestId(`theme-${id}`).click();
    const applied = await page.evaluate(() => {
      const root = getComputedStyle(document.documentElement);
      const sample = document.createElement("div");
      document.body.append(sample);
      const colors = ["--background", "--signal", "--shell-rail"].map(
        (token) => {
          sample.style.backgroundColor = `hsl(${root.getPropertyValue(token)})`;
          return getComputedStyle(sample).backgroundColor;
        },
      );
      sample.remove();
      return colors;
    });
    const previewColors = await preview.evaluate((element) => {
      const sample = document.createElement("div");
      document.body.append(sample);
      const colors = [
        element.dataset.previewSignal,
        element.dataset.previewRail,
      ].map((value) => {
        sample.style.backgroundColor = value ?? "";
        return getComputedStyle(sample).backgroundColor;
      });
      sample.remove();
      return colors;
    });
    expect([previewCanvas, ...previewColors], `${id} preview`).toEqual(applied);
  }
});

test("wallpaper can be hidden without changing the personal theme", async ({
  page,
}) => {
  await seedMarketPacks(page, ["barcelona"]);
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  const shell = page.getByTestId("chat-shell");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page.getByTestId("theme-barcelona").click();
  await expect
    .poll(() => shell.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toContain("data:image/");

  const wallpaper = page.getByTestId("settings-wallpaper");
  await wallpaper.click();
  await expect
    .poll(() => shell.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toBe("none");
  await expect.poll(() => palette(page)).toBe("barcelona");
  await page.reload();
  await expect
    .poll(() => page.evaluate(() => document.documentElement.dataset.wallpaper))
    .toBe("off");

  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await wallpaper.click();
  await expect
    .poll(() => shell.evaluate((el) => getComputedStyle(el).backgroundImage))
    .toContain("data:image/");
});

test("nature theme offers all 50 wallpapers and restores the selected scene", async ({
  page,
}) => {
  await seedMarketPacks(page, ["nature"]);
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page.getByTestId("theme-nature").click();

  const gallery = page.getByTestId("nature-wallpaper-picker");
  await expect(gallery).toBeVisible();
  await expect(gallery.getByRole("button")).toHaveCount(50);

  const selected = page.getByTestId("nature-wallpaper-050-maldives-wallpaper");
  await selected.click();
  await expect(selected).toHaveAttribute("aria-pressed", "true");
  await expect
    .poll(() =>
      page
        .getByTestId("chat-shell")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toContain("data:image/");
  await expect
    .poll(() =>
      page
        .locator(".conversation-canvas")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toBe("none");
  const sidebarColor = await page
    .getByTestId("sidebar")
    .evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(sidebarColor).toMatch(/, 0\.74\)$/);
  await expect.poll(() => palette(page)).toBe("nature");

  await page.reload();
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await expect(page.getByTestId("theme-nature")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(selected).toHaveAttribute("aria-pressed", "true");
  await page.getByTestId("settings-wallpaper").click();
  await expect
    .poll(() =>
      page
        .getByTestId("chat-shell")
        .evaluate((el) => getComputedStyle(el).backgroundImage),
    )
    .toBe("none");
});

test("Cat Acrylic theme offers 45 wallpapers and restores the selection", async ({
  page,
}) => {
  await seedMarketPacks(page, ["cat-acrylic"]);
  await login(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page.getByTestId("theme-cat-acrylic").click();
  const gallery = page.getByTestId("cat-acrylic-wallpaper-picker");
  await expect(gallery.getByRole("button")).toHaveCount(45);
  const selected = gallery.getByRole("button", { name: "Cat 45" });
  await selected.click();
  await expect(selected).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => palette(page)).toBe("cat-acrylic");
  await expect
    .poll(async () => {
      const image = await selected.locator("img").getAttribute("src");
      const background = await page
        .getByTestId("chat-shell")
        .evaluate((el) => getComputedStyle(el).backgroundImage);
      return Boolean(image && background.includes(image));
    })
    .toBe(true);

  await page.reload();
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await expect(page.getByTestId("theme-cat-acrylic")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(selected).toHaveAttribute("aria-pressed", "true");
});

test("upgrading preserves the old Cat Acrylic wallpaper choice", async ({
  page,
}) => {
  await seedMarketPacks(page, ["cat-acrylic"]);
  await page.addInitScript(() => {
    localStorage.setItem("mesh-talk-theme", "cat-acrylic");
    localStorage.setItem("mesh-talk-cat-acrylic-wallpaper", "cat-45");
  });
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await expect(page.getByTestId("theme-cat-acrylic")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(
    page.getByTestId("cat-acrylic-wallpaper-cat-45"),
  ).toHaveAttribute("aria-pressed", "true");
});

test("upgrading preserves the old Nature wallpaper choice", async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem("mesh-talk-theme", "dark");
    localStorage.setItem(
      "mesh-talk-nature-wallpaper",
      "050-maldives-wallpaper",
    );
  });
  await seedMarketPacks(page, ["nature"]);
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page.getByTestId("theme-nature").click();
  await expect(
    page.getByTestId("nature-wallpaper-050-maldives-wallpaper"),
  ).toHaveAttribute("aria-pressed", "true");
});

test("a replacement theme uses its declared base for omitted colors", async ({
  page,
}) => {
  await seedMarketPacks(page, ["barcelona"]);
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page.getByTestId("theme-barcelona").click();
  await page
    .getByTestId("pack-manager-theme")
    .locator('input[type="file"]')
    .setInputFiles({
      name: "barcelona.zip",
      mimeType: "application/zip",
      buffer: Buffer.from(
        zipSync({
          "manifest.json": strToU8(
            JSON.stringify({
              format: 1,
              id: "barcelona",
              version: "2.0.0",
              name: "Pale Barcelona",
              kind: "theme",
              base: "light",
              colors: { background: "0 0% 100%" },
            }),
          ),
        }),
      ),
    });
  await expect
    .poll(() =>
      page.evaluate(() =>
        getComputedStyle(document.documentElement)
          .getPropertyValue("--foreground")
          .trim(),
      ),
    )
    .toBe("172 20% 15%");
});

test("each theme remembers its wallpaper and previews the applied image", async ({
  page,
}) => {
  await seedMarketPacks(page, ["nature"]);
  const published = unzipSync(
    readFileSync(resolve("../site/market/packs/nature.zip")),
  );
  const images = Object.keys(published)
    .filter((name) => name.startsWith("images/") && name.endsWith(".webp"))
    .slice(0, 3);
  expect(images).toHaveLength(3);
  const custom = parsePack(
    zipSync({
      "manifest.json": strToU8(
        JSON.stringify({
          format: 1,
          id: "test.scenes",
          version: "1.0.0",
          name: "Other scenes",
          kind: "theme",
          base: "dark",
          colors: { background: "180 20% 12%" },
          wallpaper: images[2],
          wallpapers: [
            { id: "one", title: "First", file: images[0] },
            { id: "two", title: "Second", file: images[1] },
          ],
        }),
      ),
      ...Object.fromEntries(images.map((name) => [name, published[name]])),
    }),
  );
  if (custom.kind !== "theme") throw new Error("Expected a theme pack");
  await seedCustomizationPacks(page, [custom]);
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page.getByTestId("theme-nature").click();
  const natureLast = page.getByTestId(
    "nature-wallpaper-050-maldives-wallpaper",
  );
  await expect(
    page.getByTestId("nature-wallpaper-picker").getByRole("button").first(),
  ).toHaveAttribute("aria-pressed", "true");
  await natureLast.click();

  await page.getByTestId("theme-test.scenes").click();
  const customFirst = page.getByTestId("test.scenes-wallpaper-one");
  await expect(customFirst).toHaveAttribute("aria-pressed", "true");
  await expect(
    page.getByTestId("theme-preview-test.scenes").locator("img"),
  ).toHaveAttribute("src", custom.wallpapers?.[0].url ?? "");
  await page.getByTestId("test.scenes-wallpaper-two").click();
  await page.getByTestId("theme-nature").click();
  await expect(natureLast).toHaveAttribute("aria-pressed", "true");
  await page.getByTestId("theme-test.scenes").click();
  await expect(page.getByTestId("test.scenes-wallpaper-two")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.reload();
  await login(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await expect(page.getByTestId("test.scenes-wallpaper-two")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.getByTestId("theme-nature").click();
  await expect(natureLast).toHaveAttribute("aria-pressed", "true");
});

test("private wallpaper restores before the gallery and survives temporary readiness loss", async ({
  page,
}) => {
  await login(page);
  const bytes = [
    ...readFileSync(resolve("../site/market/packs/barcelona.zip")),
  ];
  await page.evaluate(async (archive) => {
    const packsPath = "/src/store/packs.ts";
    const themePath = "/src/lib/theme.ts";
    const { usePacks } = await import(/* @vite-ignore */ packsPath);
    const { useTheme } = await import(/* @vite-ignore */ themePath);
    await usePacks.getState().install(new Uint8Array(archive));
    useTheme.getState().set("barcelona");
  }, bytes);
  await page.addInitScript(() => {
    const readAll = IDBObjectStore.prototype.getAll;
    const deferred: Array<() => void> = [];
    let released = false;
    Object.assign(window, {
      releasePrivateGallery: () => {
        released = true;
        deferred.splice(0).forEach((finish) => finish());
      },
    });
    IDBObjectStore.prototype.getAll = function (...args) {
      const request = readAll.apply(this, args);
      if (
        !this.transaction.db.name.startsWith("mesh-talk-customization-account:")
      )
        return request;
      Object.defineProperty(request, "onsuccess", {
        set(callback: ((event: Event) => void) | null) {
          request.addEventListener(
            "success",
            (event) => {
              const finish = () => callback?.call(request, event);
              if (released) finish();
              else deferred.push(finish);
            },
            { once: true },
          );
        },
      });
      return request;
    };
  });
  await page.goto("/");
  await page.evaluate(() => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke(
          command: string,
          args?: Record<string, unknown>,
        ): Promise<unknown>;
      };
      releaseStartupRuntime(): void;
    };
    const invoke = host.__TAURI_INTERNALS__.invoke.bind(
      host.__TAURI_INTERNALS__,
    );
    let released = false;
    host.releaseStartupRuntime = () => {
      released = true;
    };
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "owner_node_identity" && !released)
        throw new Error("node not started");
      if (command === "owner_startup_identity") {
        const identity = (await invoke("owner_node_identity", args)) as Record<
          string,
          unknown
        >;
        return {
          ...identity,
          startup_id: "verified-private-startup",
          phase: released ? "ready" : "identity",
        };
      }
      return invoke(command, args);
    };
  });
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
  await expect.poll(() => palette(page)).toBe("barcelona");
  const runtimeReady = () =>
    page.evaluate(async () => {
      const path = "/src/store/chat.ts";
      const { useChat } = await import(/* @vite-ignore */ path);
      return useChat.getState().ready;
    });
  expect(await runtimeReady()).toBe(false);
  const snapshot = () =>
    page.evaluate(async () => {
      const path = "/src/store/packs.ts";
      const { usePacks } = await import(/* @vite-ignore */ path);
      return {
        scope: usePacks.getState().scope,
        loaded: usePacks.getState().loaded,
        wallpaper: document.documentElement.style.getPropertyValue(
          "--pack-wallpaper-url",
        ),
      };
    });
  const initial = await snapshot();
  expect(initial.loaded).toBe(false);
  expect(initial.wallpaper).toContain("data:image/");
  await page.screenshot({ path: "../tmp/wallpaper-startup-pending.png" });
  await page.evaluate(async () => {
    (
      window as unknown as { releasePrivateGallery(): void }
    ).releasePrivateGallery();
    (
      window as unknown as { releaseStartupRuntime(): void }
    ).releaseStartupRuntime();
  });
  await expect.poll(async () => (await snapshot()).loaded).toBe(true);
  await expect.poll(runtimeReady).toBe(true);
  const settled = await snapshot();
  expect(settled.wallpaper).toBe(initial.wallpaper);
  await page.evaluate(async () => {
    const path = "/src/store/chat.ts";
    const { useChat } = await import(/* @vite-ignore */ path);
    useChat.setState({ ready: false });
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  });
  expect(await snapshot()).toEqual(settled);
  await page.evaluate(async () => {
    const path = "/src/store/chat.ts";
    const { useChat } = await import(/* @vite-ignore */ path);
    useChat.setState({ ready: true });
  });
  await page.screenshot({ path: "../tmp/wallpaper-startup-ready.png" });
  await page.evaluate(async () => {
    const path = "/src/store/auth.ts";
    const { useAuth } = await import(/* @vite-ignore */ path);
    await useAuth.getState().logout();
  });
  await expect.poll(async () => (await snapshot()).scope).toBe(null);
  await expect.poll(async () => (await snapshot()).wallpaper).toBe("");
  await login(page);
  await expect
    .poll(async () => (await snapshot()).wallpaper)
    .toBe(initial.wallpaper);
  await page.evaluate(async () => {
    const path = "/src/store/chat.ts";
    const { useChat } = await import(/* @vite-ignore */ path);
    useChat.setState((state: { identityEpoch: number }) => ({
      myAccountId: "replacement-account",
      identityEpoch: state.identityEpoch + 1,
    }));
  });
  await expect
    .poll(async () => (await snapshot()).scope?.account)
    .toBe("replacement-account");
  await expect.poll(async () => (await snapshot()).wallpaper).toBe("");
});
