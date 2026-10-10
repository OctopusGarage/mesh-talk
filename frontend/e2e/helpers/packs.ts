import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";
import { parsePack } from "../../src/lib/pack";

/** Populate IndexedDB before entering the chat so optional visual scenarios stay offline. */
export async function seedMarketPacks(page: Page, ids: string[]) {
  const packs = ids.map((id) =>
    parsePack(readFileSync(resolve(`../site/market/packs/${id}.zip`))),
  );
  // This first navigation starts bundled-pack fetches. Wait for them to finish
  // before the caller navigates again; WebKit reports aborted fetches as errors.
  const bundled =
    process.env.MESH_TALK_VARIANT === "lite"
      ? []
      : (
          JSON.parse(
            readFileSync(resolve("../site/market/catalog.json"), "utf8"),
          ) as {
            id: string;
            kind: string;
            preinstall?: boolean;
          }[]
        ).filter((pack) => pack.preinstall ?? pack.kind !== "sticker");
  const bundledRequests = bundled.map(({ id }) =>
    page.waitForEvent("requestfinished", {
      predicate: (request) =>
        new URL(request.url()).pathname === `/builtin-packs/${id}.zip`,
      timeout: 45_000,
    }),
  );
  await page.goto("/");
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
  await Promise.all(bundledRequests);
}
