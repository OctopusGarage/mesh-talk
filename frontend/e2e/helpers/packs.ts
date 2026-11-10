import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";
import { parsePack, type CustomizationPack } from "../../src/lib/pack";
import type { StoredPack } from "../../src/store/packs";

/** Wait for the complete bundled import, not just the initial IndexedDB read. */
export async function waitForPackBootstrap(page: Page) {
  await page.evaluate(async () => {
    const modulePath = "/src/store/packs.ts";
    const { usePacks } = await import(/* @vite-ignore */ modulePath);
    await usePacks.getState().load();
  });
}

/** Seed the fixture's explicit account namespace; legacy storage is migration-only. */
export async function seedMarketPacks(
  page: Page,
  ids: string[],
  scope: "account" | "legacy" = "account",
) {
  const packs = ids.map((id) =>
    parsePack(readFileSync(resolve(`../site/market/packs/${id}.zip`))),
  );
  await page.goto("/");
  // Wait for the store's full load, including bundled seeding, before the
  // caller navigates again. Individual fetches may fail or be skipped.
  await waitForPackBootstrap(page);
  await seedCustomizationPacks(page, packs, scope);
}

/** Seed parsed fixtures through the same account/legacy storage contract. */
export async function seedCustomizationPacks(
  page: Page,
  packs: CustomizationPack[],
  scope: "account" | "legacy" = "account",
) {
  await page.evaluate(
    async ({ entries, scope }) => {
      const db = await new Promise<IDBDatabase>((resolve, reject) => {
        // These are the synthetic authenticated owner/account in tauri-mock.ts.
        const name =
          scope === "legacy"
            ? "mesh-talk-customization"
            : "mesh-talk-customization-account:u_self:acc_self_aaaa1111";
        const request = indexedDB.open(name, scope === "legacy" ? 2 : 1);
        request.onupgradeneeded = () => {
          if (!request.result.objectStoreNames.contains("packs"))
            request.result.createObjectStore("packs", { keyPath: "id" });
          if (
            scope === "legacy" &&
            !request.result.objectStoreNames.contains("meta")
          )
            request.result.createObjectStore("meta");
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction("packs", "readwrite");
        for (const pack of entries) {
          const row: StoredPack = {
            id: pack.id,
            pack,
            archive: null,
            sequence: 0,
            pending: false,
            version: null,
          };
          tx.objectStore("packs").put(scope === "legacy" ? pack : row);
        }
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
      });
      db.close();
    },
    { entries: packs, scope },
  );
}
