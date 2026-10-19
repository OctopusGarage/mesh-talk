import { open, save } from "@tauri-apps/plugin-dialog";
import { chat } from "./api";
import { defaultSavePath, effectiveDownloadDir } from "./download";
import { isDirectoryAttachment } from "./directoryAttachment";
import { rememberSavedDownload } from "./savedDownloads";
import {
  refreshFileAvailability,
  useFileAvailability,
} from "@/store/fileAvailability";

/** One save continuation for every attachment surface. A lease belongs to the
 * initiating chat session; readiness must still hold after any native dialog. */
export async function saveAttachment(
  file: { fileConv: string; name: string; mime?: string },
  lease: { current: () => boolean; owner?: string | null },
  options: { mode: "as" | "default"; destination?: string },
): Promise<string | null> {
  const current = () =>
    lease.current() &&
    !!useFileAvailability.getState().statuses[file.fileConv]?.ready;
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
      const defaultPath = await defaultSavePath(file.name);
      if (!current()) return null;
      const selected = directory
        ? await open({ directory: true, defaultPath })
        : await save({ defaultPath });
      if (typeof selected !== "string") return null;
      destination = selected;
    }
  }
  if (!current()) return null;
  await refreshFileAvailability([file.fileConv]);
  if (!current()) return null;
  const path = toDirectory
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
