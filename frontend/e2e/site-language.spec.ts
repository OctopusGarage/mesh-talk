import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "@playwright/test";
import { servePublicSite } from "./helpers/public-site";

const locales = [
  "en",
  "es",
  "pt-BR",
  "ja",
  "ko",
  "zh-Hans",
  "zh-Hant",
  "yue",
] as const;
type Locale = (typeof locales)[number];
const storageKey = "mesh-talk-site-language";
const subtitle =
  "Encrypted messaging across your networks. Start on a LAN, connect your sites, and keep your conversations close.";
let hosted: Awaited<ReturnType<typeof servePublicSite>>;

async function dictionary(locale: Locale): Promise<Record<string, string>> {
  return JSON.parse(
    await readFile(
      new URL(`../../site/locales/${locale}.json`, import.meta.url),
      "utf8",
    ),
  );
}

async function expectLanguage(page: Page, locale: Locale) {
  const strings = await dictionary(locale);
  await page.waitForFunction(() => "MeshTalkLocale" in window);
  await page.evaluate(() => {
    const owner = (
      window as unknown as { MeshTalkLocale: { ready: Promise<void> } }
    ).MeshTalkLocale;
    return owner.ready;
  });
  await expect(page.locator("html")).toHaveAttribute("lang", locale);
  await expect(page.locator("#site-language")).toHaveValue(locale);
  await expect(page.locator(".hero h1")).toContainText(
    strings["Your conversations."],
  );
  await expect(page.locator(".hero h1 span")).toHaveText(
    strings["On your terms."],
  );
  await expect(page.locator(".hero__sub")).toHaveText(strings[subtitle]);
  await expect(page.locator("#marketplace h2")).toContainText(
    strings["Your space."],
  );
  await expect(page.locator("footer")).toContainText("© 2026 OctopusGarage");
  if (locale !== "en") {
    await expect(page.locator(".hero h1")).not.toContainText(
      "Your conversations.",
    );
    await expect(page.locator(".hero__sub")).not.toHaveText(subtitle);
  }
}

test.beforeAll(async () => {
  hosted = await servePublicSite();
});
test.afterAll(async () => {
  await hosted?.close();
});
test.beforeEach(async ({ page }) => {
  await page.route("https://api.github.com/repos/**", (route) =>
    route.fulfill({ status: 503, json: { message: "Offline fixture" } }),
  );
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "languages", {
      value: ["en"],
      configurable: true,
    });
  });
});

for (const locale of locales) {
  test(`the language chooser renders complete ${locale} public content`, async ({
    page,
  }) => {
    await page.goto(`${hosted.url}?lang=en`);
    const chooser = page.locator("#site-language");
    await expect(chooser).toBeVisible();
    expect(
      await chooser
        .locator("option")
        .evaluateAll((options) =>
          options.map((option) => (option as HTMLOptionElement).value),
        ),
    ).toEqual(locales);
    if (locale === "pt-BR")
      await expect(chooser.locator('option[value="pt-BR"]')).toHaveText(
        "Português",
      );
    expect(
      await chooser
        .locator("option")
        .evaluateAll((options) =>
          options.every(
            (option) =>
              option.getAttribute("lang") === option.getAttribute("value"),
          ),
        ),
    ).toBe(true);
    await chooser.selectOption(locale);
    await expectLanguage(page, locale);
  });
}

test("cold dictionary completion owns readiness before language content assertions", async ({
  page,
}) => {
  const traditional = await dictionary("zh-Hant");
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let requested!: () => void;
  const requestArrived = new Promise<void>((resolve) => {
    requested = resolve;
  });
  let finished!: () => void;
  const responseFinished = new Promise<void>((resolve) => {
    finished = resolve;
  });
  let requests = 0;
  await page.route("**/locales/zh-Hant.json", async (route) => {
    requests++;
    requested();
    await held;
    try {
      await route.fulfill({ json: traditional });
    } finally {
      finished();
    }
  });
  let releaseTimer: ReturnType<typeof setTimeout> | undefined;
  let clockInstalled = false;
  try {
    const start = Date.now();
    await page.clock.install({ time: start });
    clockInstalled = true;
    await page.goto(`${hosted.url}?lang=en`);
    await page.clock.pauseAt(start + 60_000);
    await page.locator("#site-language").selectOption("zh-Hant");
    await requestArrived;
    await expect(page.locator("#language-status")).toHaveText(
      "Loading language…",
    );
    await expect(page.locator("html")).toHaveAttribute("lang", "en");
    // Hold the real response beyond the Node-side assertion's five seconds.
    // Pause browser deadlines so setup scheduling cannot consume the margin;
    // the separate deadline case verifies the unchanged eight-second abort.
    releaseTimer = setTimeout(release, 6000);
    await expectLanguage(page, "zh-Hant");
    expect(requests).toBe(1);
  } finally {
    clearTimeout(releaseTimer);
    release();
    if (requests) await responseFinished;
    if (clockInstalled) await page.clock.resume();
  }
});

test("a chosen language persists across reload with query and hash preserved", async ({
  page,
}) => {
  await page.goto(`${hosted.url}?campaign=community#marketplace`);
  await page.locator("#site-language").selectOption("zh-Hant");
  await expectLanguage(page, "zh-Hant");
  const selected = new URL(page.url());
  expect(selected.searchParams.get("lang")).toBe("zh-Hant");
  expect(selected.searchParams.get("campaign")).toBe("community");
  expect(selected.hash).toBe("#marketplace");
  expect(
    await page.evaluate((key) => localStorage.getItem(key), storageKey),
  ).toBe("zh-Hant");
  await page.reload();
  await expectLanguage(page, "zh-Hant");
  await page.goto(hosted.url);
  await expectLanguage(page, "zh-Hant");
});

test("an explicit URL language wins over saved and browser preferences", async ({
  page,
}) => {
  await page.addInitScript((key) => {
    localStorage.setItem(key, "ja");
    Object.defineProperty(navigator, "languages", {
      value: ["es-MX", "en"],
      configurable: true,
    });
  }, storageKey);
  await page.goto(`${hosted.url}?lang=zh-Hans`);
  await expectLanguage(page, "zh-Hans");
});

test("saved preference wins over the browser language", async ({ page }) => {
  await page.addInitScript((key) => {
    localStorage.setItem(key, "yue");
    Object.defineProperty(navigator, "languages", {
      value: ["ja-JP"],
      configurable: true,
    });
  }, storageKey);
  await page.goto(hosted.url);
  await expectLanguage(page, "yue");
});

test("the browser language is used when no explicit or saved choice exists", async ({
  page,
}) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "languages", {
      value: ["es-MX", "en"],
      configurable: true,
    });
  });
  await page.goto(hosted.url);
  await expectLanguage(page, "es");
});

test("unsupported preferences retain a readable English page", async ({
  page,
}) => {
  await page.addInitScript((key) => {
    localStorage.setItem(key, "unsupported");
    Object.defineProperty(navigator, "languages", {
      value: ["unsupported"],
      configurable: true,
    });
  }, storageKey);
  await page.goto(`${hosted.url}?lang=unsupported`);
  await expectLanguage(page, "en");
});

test("failed dictionaries preserve the previous language and can be retried", async ({
  page,
}) => {
  await page.goto(`${hosted.url}?lang=ja`);
  await expectLanguage(page, "ja");
  await page.route("**/locales/es.json", (route) =>
    route.fulfill({ status: 503, json: { error: "Temporary failure" } }),
  );
  await page.locator("#site-language").selectOption("es");
  const japanese = await dictionary("ja");
  await expect(page.locator("#language-status")).toHaveText(
    japanese["This language couldn’t load. Try choosing it again."],
  );
  await expectLanguage(page, "ja");
  await page.unroute("**/locales/es.json");
  await page.locator("#site-language").selectOption("es");
  await expectLanguage(page, "es");
});

test("the loader deadline settles failure without certifying translation", async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let finished!: () => void;
  const responseFinished = new Promise<void>((resolve) => {
    finished = resolve;
  });
  let requested = false;
  await page.route("**/locales/es.json", async (route) => {
    requested = true;
    await held;
    try {
      await route.abort();
    } finally {
      finished();
    }
  });
  try {
    await page.goto(`${hosted.url}?lang=en`);
    await page.clock.install();
    const requestArrived = page.waitForRequest("**/locales/es.json");
    await page.locator("#site-language").selectOption("es");
    await requestArrived;
    await expect(page.locator("#language-status")).toHaveText(
      "Loading language…",
    );
    await page.clock.fastForward(8000);
    await expectLanguage(page, "en");
    await expect(page.locator("#language-status")).toHaveText(
      "This language couldn’t load. Try choosing it again.",
    );
  } finally {
    release();
    if (requested) await responseFinished;
  }
});

test("an incomplete dictionary cannot leave a partially translated page", async ({
  page,
}) => {
  await page.goto(`${hosted.url}?lang=ja`);
  await expectLanguage(page, "ja");
  await page.route("**/locales/es.json", (route) =>
    route.fulfill({ json: { "Your conversations.": "Partial translation" } }),
  );
  await page.locator("#site-language").selectOption("es");
  const japanese = await dictionary("ja");
  await expect(page.locator("#language-status")).toHaveText(
    japanese["This language couldn’t load. Try choosing it again."],
  );
  await expectLanguage(page, "ja");
});

test("the latest choice wins when a previous dictionary arrives late", async ({
  page,
}) => {
  let releaseDictionary!: () => void;
  const held = new Promise<void>((resolve) => {
    releaseDictionary = resolve;
  });
  let requestedJapanese!: () => void;
  const japaneseRequested = new Promise<void>((resolve) => {
    requestedJapanese = resolve;
  });
  let releaseSpanish!: () => void;
  const spanishHeld = new Promise<void>((resolve) => {
    releaseSpanish = resolve;
  });
  let requestedSpanish!: () => void;
  const spanishRequested = new Promise<void>((resolve) => {
    requestedSpanish = resolve;
  });
  const japanese = await dictionary("ja");
  const spanish = await dictionary("es");
  await page.route("**/locales/ja.json", async (route) => {
    requestedJapanese();
    await held;
    await route.fulfill({ json: japanese });
  });
  await page.route("**/locales/es.json", async (route) => {
    requestedSpanish();
    await spanishHeld;
    await route.fulfill({ json: spanish });
  });
  await page.goto(`${hosted.url}?lang=en`);
  await page.locator("#site-language").selectOption("ja");
  await japaneseRequested;
  const previous = await page.evaluateHandle(() => ({
    pending: (window as unknown as { MeshTalkLocale: { ready: Promise<void> } })
      .MeshTalkLocale.ready,
  }));
  try {
    await page.locator("#site-language").selectOption("es");
    await spanishRequested;
    expect(
      await page.evaluate(async ({ pending: earlier }) => {
        const current = (
          window as unknown as { MeshTalkLocale: { ready: Promise<void> } }
        ).MeshTalkLocale.ready;
        let settled = false;
        void current.then(() => {
          settled = true;
        });
        await earlier;
        await Promise.resolve();
        return { same: current === earlier, settled };
      }, previous),
    ).toEqual({ same: false, settled: false });
    releaseSpanish();
    await expectLanguage(page, "es");
    releaseDictionary();
    await expectLanguage(page, "es");
    expect(new URL(page.url()).searchParams.get("lang")).toBe("es");
    expect(
      await page.evaluate((key) => localStorage.getItem(key), storageKey),
    ).toBe("es");
  } finally {
    releaseDictionary();
    releaseSpanish();
    await previous.dispose();
  }
});

test("blocked storage still supports URL language and internal navigation", async ({
  page,
}) => {
  await page.addInitScript(() => {
    for (const method of ["getItem", "setItem"]) {
      Object.defineProperty(Storage.prototype, method, {
        value: () => {
          throw new DOMException("Storage unavailable", "SecurityError");
        },
      });
    }
  });
  await page.goto(`${hosted.url}?lang=ja&campaign=community`);
  await expectLanguage(page, "ja");
  await page.locator("#site-language").selectOption("yue");
  await expectLanguage(page, "yue");
  const guide = page.locator('.nav__links a[href*="help/index.html"]');
  const guideUrl = new URL((await guide.getAttribute("href"))!, page.url());
  expect(guideUrl.searchParams.get("lang")).toBe("yue");
  await page.locator('.nav__links a[href="#faq"]').click();
  expect(new URL(page.url()).hash).toBe("#faq");
  expect(new URL(page.url()).searchParams.get("lang")).toBe("yue");
  await page.reload();
  await expectLanguage(page, "yue");
});

for (const [browserTag, locale] of [
  ["pt-BR", "pt-BR"],
  ["pt-PT", "pt-BR"],
  ["ko-KR", "ko"],
] as const) {
  test(`${browserTag} browser preference selects ${locale}`, async ({
    page,
  }) => {
    await page.addInitScript((tag) => {
      Object.defineProperty(navigator, "languages", {
        value: ["unsupported", tag],
        configurable: true,
      });
    }, browserTag);
    await page.goto(hosted.url);
    await expectLanguage(page, locale);
  });
}

for (const [tag, locale] of [
  ["pt-PT", "pt-BR"],
  ["ko-KR", "ko"],
] as const) {
  test(`${locale} fits mobile layouts and resolves regional URLs on recovery pages`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${hosted.url}?lang=${tag}`);
    await expectLanguage(page, locale);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth - innerWidth,
      ),
    ).toBeLessThanOrEqual(1);
    await page.locator("#site-language").selectOption("en");
    await expectLanguage(page, "en");
    await page.locator("#site-language").selectOption(locale);
    await expectLanguage(page, locale);
    await page.goto(`${hosted.url}404.html?lang=${tag}`);
    await expect(page.locator("html")).toHaveAttribute("lang", locale);
    await expect(page.locator("#site-language")).toHaveValue(locale);
  });
}

test("automatic language detection stays automatic until the visitor makes a choice", async ({
  page,
}) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "languages", {
      value: ["ja-JP"],
      configurable: true,
    });
  });
  await page.goto(hosted.url);
  await expectLanguage(page, "ja");
  expect(
    await page.evaluate((key) => localStorage.getItem(key), storageKey),
  ).toBeNull();
  await page.locator("#site-language").selectOption("en");
  await expectLanguage(page, "en");
  expect(
    await page.evaluate((key) => localStorage.getItem(key), storageKey),
  ).toBe("en");
  await page.reload();
  await expectLanguage(page, "en");
});

test("explicit English survives internal navigation when preference storage is blocked", async ({
  page,
}) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "languages", {
      value: ["ja-JP"],
      configurable: true,
    });
    for (const method of ["getItem", "setItem"])
      Object.defineProperty(Storage.prototype, method, {
        value: () => {
          throw new DOMException("Unavailable", "SecurityError");
        },
      });
  });
  await page.goto(new URL("404.html?lang=en", hosted.url).href);
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  const home = page.locator(".not-found .brand");
  await expect(home).toHaveAttribute("href", /\?lang=en$/);
  await home.click();
  await expectLanguage(page, "en");
});
