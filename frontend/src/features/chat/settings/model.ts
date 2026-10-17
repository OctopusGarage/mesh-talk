export const SETTINGS_CATEGORIES = [
  "general",
  "appearance",
  "notifications",
  "connection",
  "privacy",
  "files",
  "history",
] as const;
export type SettingsCategory = (typeof SETTINGS_CATEGORIES)[number];
export type InactiveReason =
  | "loading"
  | "saving"
  | "relayOff"
  | "physicalOnly"
  | "invisible"
  | "privacyUnknown";

export function retentionTightens(previous: number, next: number): boolean {
  return (
    Number.isFinite(next) && next > 0 && (previous === 0 || next < previous)
  );
}

/** Saved preferences are retained; this projection explains effective availability. */
export function networkControlState(input: {
  loaded: boolean;
  saving: boolean;
  relayEnabled: boolean;
  physicalOnly: boolean;
  invisible: boolean | null;
  send: boolean;
  receive: boolean;
}) {
  const base: InactiveReason | null = !input.loaded
    ? "loading"
    : input.saving
      ? "saving"
      : null;
  const relayReason: InactiveReason | null =
    base ?? (!input.relayEnabled ? "relayOff" : null);
  const directoryReason: InactiveReason | null =
    relayReason ??
    (input.physicalOnly
      ? "physicalOnly"
      : input.invisible === null
        ? "privacyUnknown"
        : input.invisible
          ? "invisible"
          : null);
  return {
    relay: { disabled: relayReason !== null, reason: relayReason },
    directory: { disabled: directoryReason !== null, reason: directoryReason },
    partialReceive: input.relayEnabled && !input.send && input.receive,
  };
}
