import { expect, test } from "./tauri-mock";
import { BOB, CHANNEL, enterChat } from "./helpers/session";

test("roster reception updates presence without reloading unrelated state", async ({
  page,
}) => {
  await page.addInitScript(() => {
    type Scope = {
      owner: string;
      account_id: string;
      device_id: string;
      runtime_id: string;
    };
    const host = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __mockEmit: (event: string, payload: unknown) => void;
      __resourceProbe: {
        counts: Record<string, number>;
        pending: number;
        next: "roster" | "state" | "documents" | "sync_status" | null;
        scope?: Scope;
        wake: (kind: "roster" | "state" | "documents" | "sync_status") => void;
      };
    };
    const probe = {
      counts: {} as Record<string, number>,
      pending: 0,
      next: null as "roster" | "state" | "documents" | "sync_status" | null,
      scope: undefined as Scope | undefined,
      wake(kind: "roster" | "state" | "documents" | "sync_status") {
        if (!probe.scope) throw new Error("No installed reception scope");
        probe.next = kind;
        host.__mockEmit("node-reception-wake", probe.scope);
      },
    };
    host.__resourceProbe = probe;
    const original = host.__TAURI_INTERNALS__.invoke;
    host.__TAURI_INTERNALS__.invoke = async (command, args) => {
      probe.counts[command] = (probe.counts[command] ?? 0) + 1;
      probe.pending++;
      try {
        const result = await original(command, args);
        if (command === "owner_node_identity") {
          probe.scope = result as Scope;
          return { ...(result as object), supports_push_state: true };
        }
        if (command === "owner_reception_page" && probe.next) {
          const kind = probe.next;
          probe.next = null;
          const value = result as { receipts: unknown[] };
          return {
            ...value,
            receipts: [
              ...value.receipts,
              { kind: "state", event: { kinds: [kind] } },
            ],
          };
        }
        return result;
      } finally {
        probe.pending--;
      }
    };
  });
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  const header = page.getByTestId("conversation-header");
  await expect(header).toContainText("3 members · 2 known online");
  const snapshot = () =>
    page.evaluate(() => {
      const probe = (
        window as unknown as {
          __resourceProbe: { counts: Record<string, number>; pending: number };
        }
      ).__resourceProbe;
      return { counts: { ...probe.counts }, pending: probe.pending };
    });
  await expect.poll(async () => (await snapshot()).pending).toBe(0);
  const before = (await snapshot()).counts;
  await page.evaluate((account) => {
    const host = window as unknown as {
      __mockSetPresence: (map: Record<string, unknown>) => void;
    };
    host.__mockSetPresence({
      [account]: {
        online: false,
        last_seen_secs: 0,
        devices: [],
        online_expires_in_secs: 0,
        reconnect_expires_in_secs: 0,
      },
    });
  }, BOB.account);
  for (let n = 0; n < 3; n++) {
    const previous = (await snapshot()).counts.owner_reception_ack ?? 0;
    await page.evaluate(() => {
      (
        window as unknown as {
          __resourceProbe: { wake: (kind: "roster") => void };
        }
      ).__resourceProbe.wake("roster");
    });
    await expect
      .poll(async () => (await snapshot()).counts.owner_reception_ack ?? 0)
      .toBeGreaterThan(previous);
    await expect.poll(async () => (await snapshot()).pending).toBe(0);
  }
  await expect(header).toContainText("3 members · 1 known online");
  const after = (await snapshot()).counts;
  expect(after.get_presence).toBeGreaterThan(before.get_presence ?? 0);
  for (const command of [
    "list_channels",
    "get_account_documents",
    "load_conversation_drafts",
    "get_account_preferences",
    "reconcile_account_settings",
    "get_favorites",
    "get_hidden_contacts",
    "note_activity",
    "set_call_active",
  ]) {
    expect(after[command] ?? 0, command).toBe(before[command] ?? 0);
  }
  // Exercise the same acknowledged reception path for qualified local changes.
  for (const kind of ["sync_status", "documents"] as const) {
    const narrowBefore = (await snapshot()).counts;
    await page.evaluate((kind) => {
      (
        window as unknown as {
          __resourceProbe: {
            wake: (kind: "sync_status" | "documents") => void;
          };
        }
      ).__resourceProbe.wake(kind);
    }, kind);
    await expect
      .poll(async () => (await snapshot()).counts.owner_reception_ack ?? 0)
      .toBeGreaterThan(narrowBefore.owner_reception_ack ?? 0);
    if (kind === "documents") {
      await expect
        .poll(
          async () => (await snapshot()).counts.load_conversation_drafts ?? 0,
        )
        .toBeGreaterThan(narrowBefore.load_conversation_drafts ?? 0);
    }
    await expect.poll(async () => (await snapshot()).pending).toBe(0);
    await page.evaluate(
      () => new Promise<void>((done) => requestAnimationFrame(() => done())),
    );
    await expect.poll(async () => (await snapshot()).pending).toBe(0);
    const narrowAfter = (await snapshot()).counts;
    for (const command of [
      "list_channels",
      "channel_members",
      "owner_personal_conversation_states",
    ]) {
      expect(narrowAfter[command] ?? 0, `${kind}: ${command}`).toBe(
        narrowBefore[command] ?? 0,
      );
    }
    if (kind === "sync_status") {
      for (const command of [
        "get_account_documents",
        "load_conversation_drafts",
      ]) {
        expect(narrowAfter[command] ?? 0, `${kind}: ${command}`).toBe(
          narrowBefore[command] ?? 0,
        );
      }
    }
  }
  await page.evaluate(() => {
    (
      window as unknown as {
        __resourceProbe: { wake: (kind: "state") => void };
      }
    ).__resourceProbe.wake("state");
  });
  await expect
    .poll(async () => (await snapshot()).counts.list_channels ?? 0)
    .toBeGreaterThan(after.list_channels ?? 0);
  await expect
    .poll(async () => (await snapshot()).counts.get_account_documents ?? 0)
    .toBeGreaterThan(after.get_account_documents ?? 0);
});

test.describe("live date display", () => {
  test.use({ timezoneId: "" });

  test("date display cache follows live timezone, midnight, and locale changes", async ({
    page,
    context,
  }) => {
    await page.goto("/");
    await page.clock.install({ time: new Date("2030-03-08T04:30:00Z") });
    const session = await context.newCDPSession(page);
    try {
      // Keep the same document and ESM module/cache throughout zone changes,
      // including DST boundaries and local dates on opposite sides of midnight.
      for (const timezoneId of [
        "UTC",
        "Asia/Shanghai",
        "America/New_York",
        "UTC",
      ]) {
        await session.send("Emulation.setTimezoneOverride", { timezoneId });
        for (const language of ["en", "ko", "pt-BR"]) {
          const results = await page.evaluate(async (language) => {
            const formatPath = "/src/lib/format.ts";
            const i18nPath = "/src/lib/i18n.ts";
            const { formatTime, formatDay } = await import(
              /* @vite-ignore */ formatPath
            );
            const { default: i18n } = await import(/* @vite-ignore */ i18nPath);
            await i18n.changeLanguage(language);
            const instants = [
              "2020-01-01T00:30:00Z",
              "2020-06-01T00:30:00Z",
              "2026-03-08T06:30:00Z",
              "2026-03-08T07:30:00Z",
              "2026-11-01T05:30:00Z",
              "2026-11-01T06:30:00Z",
            ];
            const comparisons = instants.map((instant) => {
              const date = new Date(instant);
              return {
                time: formatTime(date.getTime()),
                expectedTime: date.toLocaleTimeString(i18n.resolvedLanguage, {
                  hour: "2-digit",
                  minute: "2-digit",
                }),
                day: formatDay(date.getTime()),
                expectedDay: date.toLocaleDateString(i18n.resolvedLanguage, {
                  month: "short",
                  day: "numeric",
                }),
              };
            });
            const relative = new Intl.RelativeTimeFormat(
              i18n.resolvedLanguage,
              {
                numeric: "auto",
                style: "narrow",
              },
            );
            for (const offset of [0, -1]) {
              const date = new Date();
              date.setDate(date.getDate() + offset);
              const label = relative
                .format(offset, "day")
                .replace(/^./u, (initial) =>
                  initial.toLocaleUpperCase(i18n.resolvedLanguage),
                );
              comparisons.push({
                time: formatTime(date.getTime()),
                expectedTime: date.toLocaleTimeString(i18n.resolvedLanguage, {
                  hour: "2-digit",
                  minute: "2-digit",
                }),
                day: formatDay(date.getTime()),
                expectedDay: label,
              });
            }
            return comparisons;
          }, language);
          for (const result of results) {
            expect(result.time).toBe(result.expectedTime);
            expect(result.day).toBe(result.expectedDay);
          }
        }
      }
    } finally {
      await session.detach();
    }
  });
});
