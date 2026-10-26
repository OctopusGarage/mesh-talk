import { messageFavorites } from "./api/messageFavorites";
import { open, save } from "@tauri-apps/plugin-dialog";
import { chat } from "./api";
import { defaultSavePath, effectiveDownloadDir } from "./download";
import { isDirectoryAttachment } from "./directoryAttachment";
import { rememberSavedDownload } from "./savedDownloads";
import {
  refreshFileAvailability,
  useFileAvailability,
} from "@/store/fileAvailability";
import { decodeStickerMedia } from "./stickerMedia";

interface Attachment {
  fileConv: string;
  name: string;
  mime?: string;
  media?: boolean;
  size?: number;
}

/** Export a recognized sticker as an image with a legal, human-readable basename. */
function exportName(file: Attachment): string {
  const sticker = decodeStickerMedia({
    ...file,
    media: file.media ?? false,
    size: file.size ?? 0,
  });
  if (!sticker) return file.name;
  let label =
    (sticker.label ?? "sticker")
      .replace(/[<>:"/\\|?*]/gu, "_")
      .replace(/^[. ]+|[. ]+$/gu, "") || "sticker";
  if (
    /^(?:CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[0-9¹²³]|LPT[0-9¹²³])(?:\.|$)/iu.test(
      label,
    )
  )
    label = `_${label}`;
  return `${label}.${file.name.split(".").pop()}`;
}

/** One save continuation for every attachment surface. A lease belongs to the
 * initiating chat session; readiness must still hold after any native dialog. */
export async function saveAttachment(
  file: Attachment,
  lease: { current: () => boolean; owner?: string | null },
  options: {
    mode: "as" | "default";
    destination?: string;
    source?: "favorite";
  },
): Promise<string | null> {
  const favorite = options.source === "favorite";
  let retainedReady = false;
  const refresh = async () => {
    if (!lease.current()) return;
    if (favorite)
      retainedReady =
        !!lease.owner &&
        (await messageFavorites.fileAvailable(lease.owner, file.fileConv));
    else await refreshFileAvailability([file.fileConv]);
  };
  const current = () =>
    lease.current() &&
    (favorite
      ? retainedReady
      : !!useFileAvailability.getState().statuses[file.fileConv]?.ready);
  if (favorite) await refresh();
  if (!current()) return null;
  const directory = isDirectoryAttachment(file.mime ?? "");
  let destination = options.destination;
  let toDirectory = directory;
  if (destination === undefined) {
    if (options.mode === "default") {
      const dir = await effectiveDownloadDir();
      if (!current()) return null;
      if (dir) {
        destination = dir;
        toDirectory = true;
      }
    }
    if (destination === undefined) {
      const defaultPath = await defaultSavePath(exportName(file));
      if (!current()) return null;
      const selected = directory
        ? await open({ directory: true, defaultPath })
        : await save({ defaultPath });
      if (typeof selected !== "string") return null;
      destination = selected;
    }
  }
  if (!current()) return null;
  await refresh();
  if (!current()) return null;
  const path = favorite
    ? await messageFavorites.saveFile(
        lease.owner!,
        file.fileConv,
        destination,
        toDirectory,
      )
    : toDirectory
      ? await chat.saveFileToDir(
          file.fileConv,
          destination,
          lease.owner ?? undefined,
        )
      : (await chat.saveFile(file.fileConv, destination), destination);
  if (!lease.current()) return null;
  rememberSavedDownload(file.fileConv, path);
  return path;
}
