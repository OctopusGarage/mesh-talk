import type { Page } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { contrastRatio } from "./helpers/ui-audit";

const BOB = "acc_bob_bbbb2222";
const SENT_URL = "https://sent.example.test";
const RECEIVED_URL = "https://received.example.test";
const THEMES = ["light", "dark", "oled", "argentina", "barcelona", "messi"];
const MIN_TEXT_CONTRAST = 4.5;

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
        const isPalette = ["argentina", "barcelona", "messi"].includes(theme);
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
    expect(check.bodyOverflows, `${check.theme} body overflow`).toBe(false);
    expect(check.bubbleOverflows, `${check.theme} bubble overflow`).toBe(false);
  }
});
