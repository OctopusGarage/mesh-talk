// Built-in avatar packs the user can pick from, instead of uploading a photo. Four sets:
// football clubs + NBA teams for GROUP (channel) avatars, and football stars + NBA stars
// for PERSONAL avatars. The manifests are built automatically from the bundled assets via
// `import.meta.glob` (eager URL imports) — drop a file in the folder and it shows up.

export interface AvatarPreset {
  /** Display name shown under the thumbnail. */
  label: string;
  /** Bundled asset URL (hashed by Vite). */
  url: string;
}

/** Derive a label from a preset filename: drop the extension + any leading "NN-" rank
 *  prefix; turn a kebab name into spaced Title Case (club logos), or keep an
 *  already-spaced display name (player photos). */
function labelFromPath(path: string): string {
  const base = (path.split("/").pop() ?? "").replace(/\.[^.]+$/, "");
  const noRank = base.replace(/^\d+-/, "");
  if (noRank.includes(" ")) return noRank;
  return noRank
    .split("-")
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

function pack(modules: Record<string, string>): AvatarPreset[] {
  return Object.entries(modules)
    .sort(([a], [b]) => a.localeCompare(b)) // by path: clubs keep rank order, players A→Z
    .map(([path, url]) => ({ label: labelFromPath(path), url }));
}

/** Club logos — preset GROUP (channel) avatars. */
export const CLUB_AVATARS: AvatarPreset[] = pack(
  import.meta.glob("../assets/avatars/clubs/*.svg", {
    eager: true,
    query: "?url",
    import: "default",
  }),
);

/** Football-star photos — preset PERSONAL avatars. */
export const PLAYER_AVATARS: AvatarPreset[] = pack(
  import.meta.glob("../assets/avatars/players/*.webp", {
    eager: true,
    query: "?url",
    import: "default",
  }),
);

export type AvatarPackName = "clubs" | "players" | "nba-players" | "nba-teams";

export function avatarPack(name: AvatarPackName): AvatarPreset[] {
  switch (name) {
    case "clubs":
      return CLUB_AVATARS;
    case "players":
      return PLAYER_AVATARS;
    case "nba-teams":
      return NBA_TEAM_AVATARS;
    case "nba-players":
      return NBA_PLAYER_AVATARS;
  }
}

/** NBA team logos — preset GROUP (channel) avatars. */
export const NBA_TEAM_AVATARS: AvatarPreset[] = pack(
  import.meta.glob("../assets/avatars/nba-teams/*.webp", {
    eager: true,
    query: "?url",
    import: "default",
  }),
);

/** NBA star photos — preset PERSONAL avatars. */
export const NBA_PLAYER_AVATARS: AvatarPreset[] = pack(
  import.meta.glob("../assets/avatars/nba-players/*.webp", {
    eager: true,
    query: "?url",
    import: "default",
  }),
);
