import { decodeStickerMedia } from "./stickerMedia";

/** The sender uses a regular tar attachment so older clients can still download it. */
export const DIRECTORY_MIME = "application/x-mesh-talk-directory-tar";

export function isDirectoryAttachment(mime: string): boolean {
  return mime === DIRECTORY_MIME;
}

export function attachmentLabel(
  name: string,
  mime: string,
  metadata?: { media: boolean; size: number },
): string {
  const sticker = metadata
    ? decodeStickerMedia({ name, mime, ...metadata })
    : null;
  if (sticker) return sticker.fallback;
  return isDirectoryAttachment(mime) && name.endsWith(".tar")
    ? name.slice(0, -4)
    : name;
}
