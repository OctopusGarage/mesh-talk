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
        next: "roster" | "state" | null;
        scope?: Scope;
        wake: (kind: "roster" | "state") => void;
      };
    };
    const probe = {
      counts: {} as Record<string, number>,
      pending: 0,
      next: null as "roster" | "state" | null,
      scope: undefined as Scope | undefined,
      wake(kind: "roster" | "state") {
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
