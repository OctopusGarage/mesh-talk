import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";
import { parsePack } from "../../src/lib/pack";

/** Include bundled seeding and image validation, rather than only the first DB read. */
export async function waitForPacks(page: Page) {
  await page.evaluate(async () => {
    const modulePath = "/src/store/packs.ts";
    const { usePacks } = await import(/* @vite-ignore */ modulePath);
    await usePacks.getState().load();
  });
}

/** Populate IndexedDB before entering the chat so optional visual scenarios stay offline. */
export async function seedMarketPacks(page: Page, ids: string[]) {
  const packs = ids.map((id) =>
    parsePack(readFileSync(resolve(`../site/market/packs/${id}.zip`))),
  );
  await page.goto("/");
  await waitForPacks(page);
  await page.evaluate(async (entries) => {
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("mesh-talk-customization", 2);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains("packs"))
          request.result.createObjectStore("packs", { keyPath: "id" });
        if (!request.result.objectStoreNames.contains("meta"))
          request.result.createObjectStore("meta");
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction("packs", "readwrite");
      for (const pack of entries) tx.objectStore("packs").put(pack);
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    });
    db.close();
  }, packs);
}
