import { chat } from "@/lib/api/chat";
import { appearanceValues, applyLinkedAppearance, useTheme } from "@/lib/theme";
import { createLinkedPreferencePublisher } from "./linkedPreferencePublisher";

/** Preserve the existing device's saved look before it issues a pairing code. */
export async function publishAppearanceBeforeLink(): Promise<void> {
  const existing = new Set(
    (await chat.getAccountPreferences("appearance")).map(
      (record) => record.key,
    ),
  );
  for (const [key, value] of Object.entries(appearanceValues())) {
    if (!existing.has(key))
      await chat.setAccountPreference("appearance", key, value);
  }
}

/** Reconcile the signed-in account's appearance with the local theme store. */
export function startLinkedAppearance(): () => void {
  let active = true;
  let polling = false;
  let applying = false;
  const publisher = createLinkedPreferencePublisher(
    (key, value) => chat.setAccountPreference("appearance", key, value),
    (error) => console.warn("Could not save linked appearance", error),
  );
  let previous = appearanceValues();
  const unsubscribe = useTheme.subscribe(() => {
    const next = appearanceValues();
    if (active && !applying) {
      for (const [key, value] of Object.entries(next)) {
        if (previous[key] !== value) publisher.publish(key, value);
      }
    }
    previous = next;
  });
  const reconcile = async () => {
    if (polling || !active) return;
    polling = true;
    try {
      const [policy, records] = await Promise.all([
        chat.getDeviceSyncPolicy(),
        chat.getAccountPreferences("appearance"),
      ]);
      if (!active) return;
      publisher.setEnabled(policy.appearance);
      if (policy.appearance) {
        applying = true;
        try {
          for (const record of records) {
            if (publisher.shouldApply(record.key, record.revision)) {
              applyLinkedAppearance(record.key, record.value);
              publisher.markApplied(record.key, record.revision);
            }
          }
        } finally {
          applying = false;
          previous = appearanceValues();
        }
      }
    } catch (error) {
      // The browser preview has no native node; a later poll retries desktop failures.
      if (import.meta.env.PROD)
        console.warn("Could not reconcile linked appearance", error);
    } finally {
      polling = false;
    }
  };
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), 10_000);
  return () => {
    active = false;
    publisher.stop();
    unsubscribe();
    window.clearInterval(timer);
  };
}
