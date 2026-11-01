import { test, expect } from "./tauri-mock";
import { BOB, CHANNEL, enterChat, openBobDm } from "./helpers/session";

test("loaded history and conversation switches stay still while live arrivals animate", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await enterChat(page);
  await page.evaluate(() => {
    const samples: { text: string; opacity: string; transform: string }[] = [];
    Object.assign(window, { __arrivalSamples: samples });
    new MutationObserver((records) => {
      for (const record of records)
        for (const node of record.addedNodes) {
          if (!(node instanceof Element)) continue;
          const rows = node.matches('[data-testid="message-bubble"]')
            ? [node]
            : [...node.querySelectorAll('[data-testid="message-bubble"]')];
          for (const row of rows)
            samples.push({
              text: row.textContent ?? "",
              opacity: (row as HTMLElement).style.opacity,
              transform: (row as HTMLElement).style.transform,
            });
        }
    }).observe(document.body, { childList: true, subtree: true });
  });
  await openBobDm(page);
  await page.evaluate(
    ({ who, account }) => {
      const mock = window as unknown as {
        __mockInject: (
          conversation: string,
          text: string,
          who: string,
          metadata: { id: string },
        ) => void;
        __mockEmit: (event: string, payload: unknown) => void;
      };
      mock.__mockInject(`acc:${account}`, "Live motion probe", who, {
        id: "motion-live",
      });
      mock.__mockEmit("dm-received", {
        from: who,
        account_id: account,
        text: "Live motion probe",
        reply_to: null,
        message_id: "motion-live",
        arrival_kind: "live",
      });
    },
    { who: BOB.device, account: BOB.account },
  );
  await expect(
    page.getByRole("log").getByText("Live motion probe", { exact: true }),
  ).toBeVisible();
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(
    page.getByRole("log").getByText("Live motion probe", { exact: true }),
  ).toBeVisible();
  const samples = await page.evaluate(
    () =>
      (
        window as unknown as {
          __arrivalSamples: {
            text: string;
            opacity: string;
            transform: string;
          }[];
        }
      ).__arrivalSamples,
  );
  const history = samples.filter((row) =>
    row.text.includes("hey, welcome to the mesh"),
  );
  expect(history.length).toBeGreaterThanOrEqual(2);
  expect(history.every((row) => row.opacity !== "0")).toBe(true);
  const live = samples.filter((row) => row.text.includes("Live motion probe"));
  expect(live.length).toBeGreaterThanOrEqual(2);
  expect(live[0].opacity).toBe("0");
  expect(live[live.length - 1].opacity).not.toBe("0");
});

test("turning on reduced motion settles an in-flight message entrance", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await enterChat(page);
  await openBobDm(page);
  const first = await page.evaluate(
    ({ who, account }) =>
      new Promise<{ opacity: string; y: number }>((resolve) => {
        const observer = new MutationObserver(() => {
          const row = [
            ...document.querySelectorAll<HTMLElement>(
              '[data-testid="message-bubble"]',
            ),
          ].find((e) => e.textContent?.includes("Reduced motion live probe"));
          if (!row) return;
          observer.disconnect();
          resolve({
            opacity: row.style.opacity,
            y: new DOMMatrix(getComputedStyle(row).transform).m42,
          });
        });
        observer.observe(document.body, { subtree: true, childList: true });
        const mock = window as unknown as {
          __mockInject: (
            conversation: string,
            text: string,
            who: string,
            meta: { id: string },
          ) => void;
          __mockEmit: (name: string, payload: unknown) => void;
        };
        mock.__mockInject(`acc:${account}`, "Reduced motion live probe", who, {
          id: "reduce-in-flight",
        });
        mock.__mockEmit("dm-received", {
          from: who,
          account_id: account,
          text: "Reduced motion live probe",
          reply_to: null,
          message_id: "reduce-in-flight",
          arrival_kind: "live",
        });
      }),
    { who: BOB.device, account: BOB.account },
  );
  expect(first.opacity).toBe("0");
  expect(first.y).toBeGreaterThan(0);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const y = await page
    .getByTestId("message-bubble")
    .filter({ hasText: "Reduced motion live probe" })
    .evaluate(async (row) => {
      await new Promise(requestAnimationFrame);
      return new DOMMatrix(getComputedStyle(row).transform).m42;
    });
  expect(y).toBe(0);
});
