import { startRecoveryRefresh } from "./recoveryRefresh";
import {
  startLinkedCustomizationPacks,
  publishCustomizationPacksBeforeLink,
} from "./linkedCustomizationPacks";
import {
  startLinkedAvatarOverrides,
  publishAvatarOverridesBeforeLink,
} from "./linkedAvatarOverrides";
import { reconcileAccountSettings } from "./api/settings";
import { useSettings } from "@/store/settings";
import { usePacks } from "@/store/packs";
import { privacyFromSettings } from "./privacySettings";
import { asRingtoneId } from "./ringtones";
import { captureRuntimeOwner } from "@/store/ownership";
import { useChat } from "@/store/chat";
import { readWithDeadline } from "./readDeadline";
import { subscribeDocumentInvalidation } from "./syncInvalidation";
import { chat } from "@/lib/api/chat";
import {
  appearanceValues,
  savedAppearanceValues,
  applyLinkedAppearance,
  useTheme,
} from "@/lib/theme";
import { createLinkedPreferencePublisher } from "./linkedPreferencePublisher";
import {
  applyAccountLanguage,
  savedLanguage,
  subscribeLanguageChanges,
} from "./i18n";

/** Preserve the existing device's saved look before it issues a pairing code. */
export async function publishAppearanceBeforeLink(
  owner: string,
): Promise<void> {
  const lease = captureRuntimeOwner();
  const accountId = useChat.getState().myAccountId;
  if (!accountId) throw new Error("Account identity is not ready");
  const requireCurrent = () => {
    if (
      lease.owner !== owner ||
      !lease.current() ||
      useChat.getState().myAccountId !== accountId
    )
      throw new Error("Account changed during settings preparation");
  };
  requireCurrent();
  await reconcileAccountSettings(owner, accountId, true);
  requireCurrent();
  await publishCustomizationPacksBeforeLink(owner);
  requireCurrent();
  await publishAvatarOverridesBeforeLink(owner);
  requireCurrent();
  const existing = new Set(
    (await chat.getAccountPreferences("appearance", owner)).map(
      (record) => record.key,
    ),
  );
  requireCurrent();
  const saved = savedAppearanceValues();
  const language = savedLanguage();
  if (language) saved.language = language;
  for (const [key, value] of Object.entries(saved)) {
    if (!existing.has(key)) {
      if (
        lease.owner !== owner ||
        !lease.current() ||
        useChat.getState().myAccountId !== accountId
      )
        throw new Error("Account changed during appearance preparation");
      await chat.seedAccountPreference(
        "appearance",
        key,
        value,
        owner,
        accountId,
      );
    }
  }
  requireCurrent();
}

/** Reconcile the signed-in account's appearance with the local theme store. */
export function startLinkedAppearance(owner: string): () => void {
  const stopPacks = startLinkedCustomizationPacks(
    owner,
    useTheme.getState().theme,
  );
  const installedPackScope = usePacks.getState().scope;
  const stopAvatars = startLinkedAvatarOverrides(owner);
  let active = true;
  const lease = captureRuntimeOwner();
  const accountId = useChat.getState().myAccountId;
  const current = () =>
    active &&
    lease.owner === owner &&
    lease.current() &&
    Boolean(accountId) &&
    useChat.getState().myAccountId === accountId;
  let polling = false;
  let dirty = false;
  let applying = false;
  const publisher = createLinkedPreferencePublisher(
    (key, value) => {
      if (!current())
        return Promise.reject(
          new Error("Account changed during appearance synchronization"),
        );
      return chat.setAccountPreference(
        "appearance",
        key,
        value,
        owner,
        accountId,
      );
    },
    (error) => console.warn("Could not save linked appearance", error),
  );
  let previous = appearanceValues();
  const unsubscribeLanguage = subscribeLanguageChanges((language) => {
    if (current() && !applying) publisher.publish("language", language);
  });
  const unsubscribe = useTheme.subscribe(() => {
    const next = appearanceValues();
    if (current() && !applying) {
      for (const [key, value] of Object.entries(next)) {
        if (previous[key] !== value) publisher.publish(key, value);
      }
    }
    previous = next;
  });
  let installedPacks = usePacks.getState().packs;
  let selectedPack = usePacks.getState().selectedTheme;
  const unsubscribePacks = usePacks.subscribe((state) => {
    if (state.packs === installedPacks && state.selectedTheme === selectedPack)
      return;
    installedPacks = state.packs;
    selectedPack = state.selectedTheme;
    if (
      !current() ||
      ["dark", "light", "oled"].includes(useTheme.getState().theme)
    )
      return;
    applying = true;
    try {
      // A matching theme can arrive after its small preference record. Missing
      // assets use fallback CSS while retaining the account's selected ID.
      useTheme.getState().refresh(true);
    } finally {
      applying = false;
      previous = appearanceValues();
    }
  });
  const reconcile = async () => {
    if (!current()) return;
    if (polling) {
      dirty = true;
      return;
    }
    polling = true;
    try {
      const [policy, records, settings] = await Promise.all([
        readWithDeadline(() => chat.getDeviceSyncPolicy(owner)),
        readWithDeadline(() => chat.getAccountPreferences("appearance", owner)),
        readWithDeadline(() => reconcileAccountSettings(owner, accountId!)),
      ]);
      if (!current()) return;
      if (settings && useSettings.getState().owner === owner) {
        const store = useSettings.getState();
        store.setCallsEnabled(settings.calls_enabled);
        store.setNotificationsEnabled(settings.notifications);
        store.setRingtone(asRingtoneId(settings.ringtone));
        store.setPrivacy(privacyFromSettings(settings));
      }
      publisher.setEnabled(policy.appearance);
      if (policy.appearance) {
        for (const record of records) {
          if (
            !publisher.shouldApply(
              record.key,
              record.revision,
              record.device_id,
            )
          )
            continue;
          if (
            record.key === "theme" &&
            record.value &&
            !["dark", "light", "oled"].includes(record.value)
          ) {
            // Hydration observes remote state; local edits retain publication
            // authority throughout this await and can invalidate this revision.
            const prepared = await usePacks
              .getState()
              .prepareTheme(record.value);
            if (!prepared) continue;
          }
          if (!current()) return;
          if (
            !publisher.shouldApply(
              record.key,
              record.revision,
              record.device_id,
            )
          )
            continue;
          applying = true;
          try {
            if (record.key === "language") applyAccountLanguage(record.value);
            else applyLinkedAppearance(record.key, record.value);
            publisher.markApplied(
              record.key,
              record.revision,
              record.device_id,
            );
          } finally {
            applying = false;
            previous = appearanceValues();
          }
        }
      }
    } catch (error) {
      // The browser preview has no native node; a later poll retries desktop failures.
      if (import.meta.env.PROD)
        console.warn("Could not reconcile linked appearance", error);
    } finally {
      polling = false;
      if (current() && dirty) {
        dirty = false;
        queueMicrotask(() => void reconcile());
      }
    }
  };
  const unlisten = subscribeDocumentInvalidation(owner, () => {
    void reconcile();
  });
  void reconcile();
  const stopRecovery = startRecoveryRefresh(() => void reconcile());
  return () => {
    active = false;
    publisher.stop();
    const ownsPackScope = usePacks.getState().scope === installedPackScope;
    stopPacks();
    // The subscriber is already inactive: explicitly revoke private CSS when
    // this lifecycle retires, without clearing a replacement account's theme.
    if (ownsPackScope) useTheme.getState().refresh(true);
    stopAvatars();
    unsubscribe();
    unsubscribeLanguage();
    unsubscribePacks();
    unlisten();
    stopRecovery();
  };
}
