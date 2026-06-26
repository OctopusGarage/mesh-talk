import { chat, settings } from "@/lib/api";

/**
 * Where downloads go. Precedence: the folder the user explicitly chose (settings
 * `download_dir`), else the platform's standard Downloads folder (macOS `~/Downloads`,
 * Windows the Downloads known folder, Linux `XDG_DOWNLOAD_DIR` → `~/Downloads`, resolved by
 * the backend), else "" so the caller falls back to a Save-as prompt. This makes downloads
 * land in the OS Downloads folder by default — the common browser/IM behavior — without the
 * user having to configure anything.
 */

// The OS Downloads dir is stable for the session, so resolve it once and cache it.
let osDownloadDir: string | null | undefined;

async function osDownloads(): Promise<string | null> {
  if (osDownloadDir === undefined) {
    osDownloadDir = await chat.defaultDownloadDir().catch(() => null);
  }
  return osDownloadDir;
}

/** The effective download folder: the configured one, else the OS Downloads dir, else "". */
export async function effectiveDownloadDir(): Promise<string> {
  try {
    const configured = (await settings.get()).download_dir;
    if (configured) return configured;
  } catch {
    // settings unavailable — fall through to the OS default.
  }
  return (await osDownloads()) ?? "";
}

/** Join a directory and a file name using the directory's native separator. */
export function joinPath(dir: string, name: string): string {
  if (!dir) return name;
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.endsWith(sep) ? `${dir}${name}` : `${dir}${sep}${name}`;
}

/** A Save-dialog `defaultPath` that opens at the effective download folder with `name`
 * prefilled (just the bare name if no folder is resolvable). */
export async function defaultSavePath(name: string): Promise<string> {
  return joinPath(await effectiveDownloadDir(), name);
}
