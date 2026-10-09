import type { Page } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { contrastRatio } from "./helpers/ui-audit";

const BOB = "acc_bob_bbbb2222";
const SENT_URL = "https://sent.example.test";
const RECEIVED_URL = "https://received.example.test";
const THEMES = [
  "light",
  "dark",
  "oled",
  "argentina",
  "barcelona",
  "messi",
  "nature",
];
const MIN_TEXT_CONTRAST = 4.5;

test("attention text is readable on dialog surfaces in every theme", async ({
  page,
}) => {
  await page.goto("/");
  const checks = await page.evaluate((themes) => {
    const probe = document.createElement("span");
    probe.style.color = "hsl(var(--attention))";
    probe.style.backgroundColor = "hsl(var(--popover))";
    document.body.append(probe);
    return themes.map((theme) => {
      const root = document.documentElement;
      root.classList.toggle(
        "dark",
        ["dark", "oled", "barcelona"].includes(theme),
      );
      root.classList.toggle("oled", theme === "oled");
      if (["argentina", "barcelona", "messi", "nature"].includes(theme))
        root.dataset.palette = theme;
      else delete root.dataset.palette;
      const style = getComputedStyle(probe);
      return {
        theme,
        foreground: style.color,
        background: style.backgroundColor,
      };
    });
  }, THEMES);
  for (const check of checks) {
    expect(
      contrastRatio(check.foreground, check.background),
      check.theme,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
  }
});

async function enterBobDm(page: Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
}

test.use({ viewport: { width: 1280, height: 800 } });

test("message text and links stay readable across every theme", async ({
  page,
}) => {
  await enterBobDm(page);

  await page.getByTestId("composer-input").fill(`sent link ${SENT_URL}`);
  await page.getByTestId("composer-send").click();
  await expect(page.getByRole("link", { name: SENT_URL })).toBeVisible();

  await page.evaluate(
    ({ url }) => {
      const w = window as unknown as Record<string, unknown>;
      (w.__mockInject as (c: string, t: string, who: string) => void)(
        "acc:acc_bob_bbbb2222",
        `received link ${url}`,
        "device_bob_2222",
      );
      (w.__mockEmit as (e: string, p: unknown) => void)("dm-received", {
        from: "device_bob_2222",
        from_name: "bob",
        text: `received link ${url}`,
        reply_to: null,
      });
    },
    { url: RECEIVED_URL },
  );
  await expect(page.getByRole("link", { name: RECEIVED_URL })).toBeVisible();

  const checks = await page.evaluate(
    ({ themes, sentUrl, receivedUrl }) => {
      const applyTheme = (theme: string) => {
        const root = document.documentElement;
        const isPalette = [
          "argentina",
          "barcelona",
          "messi",
          "nature",
        ].includes(theme);
        const darkBase =
          theme === "dark" || theme === "oled" || theme === "barcelona";

        root.classList.toggle("dark", darkBase);
        root.classList.toggle("oled", theme === "oled");
        if (isPalette) root.setAttribute("data-palette", theme);
        else root.removeAttribute("data-palette");
      };

      const bubbleForLink = (url: string) => {
        const links = Array.from(document.querySelectorAll("a"));
        const link = links.find((candidate) => candidate.textContent === url);
        const bubble = link?.closest("[data-context-menu]");
        if (
          !(link instanceof HTMLElement) ||
          !(bubble instanceof HTMLElement)
        ) {
          throw new Error(`Missing rendered link bubble for ${url}`);
        }
        return { link, bubble };
      };

      const rows = Array.from(
        document.querySelectorAll<HTMLElement>(
          '[data-testid="message-bubble"]',
        ),
      );
      const log = document.querySelector<HTMLElement>('[role="log"]');
      if (!log) throw new Error("Missing conversation log");

      return themes.map((theme) => {
        applyTheme(theme);
        const { link: sentLink, bubble: sentBubble } = bubbleForLink(sentUrl);
        const { link: receivedLink, bubble: receivedBubble } =
          bubbleForLink(receivedUrl);

        const sentBubbleStyle = getComputedStyle(sentBubble);
        const receivedBubbleStyle = getComputedStyle(receivedBubble);
        const deliveryFooter = document.querySelector(
          '[data-delivery="awaiting"]',
        )?.parentElement;
        if (!deliveryFooter) throw new Error("Missing delivery footer");

        const logRect = log.getBoundingClientRect();
        const bodyOverflows =
          document.documentElement.scrollWidth >
          document.documentElement.clientWidth + 1;
        const bubbleOverflows = rows.some((row) => {
          const rect = row.getBoundingClientRect();
          return rect.left < logRect.left - 1 || rect.right > logRect.right + 1;
        });

        return {
          theme,
          sentTextColor: sentBubbleStyle.color,
          sentBubbleBackground: sentBubbleStyle.backgroundColor,
          sentLinkColor: getComputedStyle(sentLink).color,
          receivedLinkColor: getComputedStyle(receivedLink).color,
          receivedBubbleBackground: receivedBubbleStyle.backgroundColor,
          deliveryColor: getComputedStyle(deliveryFooter).color,
          canvasBackground: getComputedStyle(document.body).backgroundColor,
          bodyOverflows,
          bubbleOverflows,
        };
      });
    },
    {
      themes: THEMES,
      sentUrl: SENT_URL,
      receivedUrl: RECEIVED_URL,
    },
  );

  expect(checks).toEqual(
    expect.arrayContaining(
      THEMES.map((theme) => expect.objectContaining({ theme })),
    ),
  );
  for (const check of checks) {
    expect(
      contrastRatio(check.sentTextColor, check.sentBubbleBackground),
      `${check.theme} sent text`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    expect(
      contrastRatio(check.sentLinkColor, check.sentBubbleBackground),
      `${check.theme} sent link`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    expect(
      contrastRatio(check.receivedLinkColor, check.receivedBubbleBackground),
      `${check.theme} received link`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    expect(
      contrastRatio(check.deliveryColor, check.canvasBackground),
      `${check.theme} delivery metadata`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    expect(check.bodyOverflows, `${check.theme} body overflow`).toBe(false);
    expect(check.bubbleOverflows, `${check.theme} bubble overflow`).toBe(false);
  }
});

test("destructive confirmation stays readable across every theme", async ({
  page,
}) => {
  await enterBobDm(page);
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-delete").click();
  const checks = await page.evaluate((themes) => {
    const root = document.documentElement;
    const button = document.querySelector<HTMLElement>(
      '[data-testid="delete-message-confirm"]',
    );
    if (!button) throw new Error("Missing delete confirmation");
    return themes.map((theme) => {
      const palette = ["argentina", "barcelona", "messi", "nature"].includes(
        theme,
      );
      root.classList.toggle(
        "dark",
        theme === "dark" || theme === "oled" || theme === "barcelona",
      );
      root.classList.toggle("oled", theme === "oled");
      if (palette) root.setAttribute("data-palette", theme);
      else root.removeAttribute("data-palette");
      const style = getComputedStyle(button);
      return {
        theme,
        foreground: style.color,
        background: style.backgroundColor,
      };
    });
  }, THEMES);
  for (const check of checks) {
    expect(
      contrastRatio(check.foreground, check.background),
      `${check.theme} destructive confirmation`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
  }
});

test("changed identity warning stays readable across every theme", async ({
  page,
}) => {
  await page.goto("/?data=changed");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await page.getByTestId(`conversation-row-${BOB}`).click();
  await expect(page.getByTestId("verify-trigger")).toHaveAttribute(
    "data-trust",
    "changed",
  );
  await page.getByTestId("verify-trigger").click();
  const checks = await page.evaluate((themes) => {
    const root = document.documentElement;
    const dialog = document.querySelector<HTMLElement>(
      '[data-testid="verify-dialog"]',
    );
    const title = Array.from(
      dialog?.querySelectorAll<HTMLElement>("p") ?? [],
    ).find((el) => el.textContent?.includes("safety number changed"));
    const banner = title?.closest<HTMLElement>(".text-destructive");
    const body = title?.nextElementSibling as HTMLElement | null;
    if (!dialog || !banner || !title || !body)
      throw new Error("Missing changed-identity warning");
    // Measure the settled theme. The dialog's entrance transition otherwise
    // leaves its background at the previous theme during this synchronous sweep.
    dialog.style.transition = "none";
    void getComputedStyle(dialog).backgroundColor;
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("Missing canvas context");
    return themes.map((theme) => {
      const palette = ["argentina", "barcelona", "messi", "nature"].includes(
        theme,
      );
      root.classList.toggle(
        "dark",
        theme === "dark" || theme === "oled" || theme === "barcelona",
      );
      root.classList.toggle("oled", theme === "oled");
      if (palette) root.setAttribute("data-palette", theme);
      else root.removeAttribute("data-palette");
      context.fillStyle = getComputedStyle(dialog).backgroundColor;
      context.fillRect(0, 0, 1, 1);
      context.fillStyle = getComputedStyle(banner).backgroundColor;
      context.fillRect(0, 0, 1, 1);
      const [r, g, b] = context.getImageData(0, 0, 1, 1).data;
      return {
        theme,
        title: getComputedStyle(title).color,
        body: getComputedStyle(body).color,
        background: `rgb(${r}, ${g}, ${b})`,
      };
    });
  }, THEMES);
  for (const check of checks) {
    expect(
      contrastRatio(check.title, check.background),
      `${check.theme} changed-identity title`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    expect(
      contrastRatio(check.body, check.background),
      `${check.theme} changed-identity description`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
  }
});
