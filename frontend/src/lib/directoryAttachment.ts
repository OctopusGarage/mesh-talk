/** The sender uses a regular tar attachment so older clients can still download it. */
export const DIRECTORY_MIME = "application/x-mesh-talk-directory-tar";

export function isDirectoryAttachment(mime: string): boolean {
  return mime === DIRECTORY_MIME;
}

export function attachmentLabel(name: string, mime: string): string {
  return isDirectoryAttachment(mime) && name.endsWith(".tar")
    ? name.slice(0, -4)
    : name;
}
