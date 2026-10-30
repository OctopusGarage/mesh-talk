import { useSettings } from "@/store/settings";

export function canCopyImage(): boolean {
  return (
    typeof ClipboardItem !== "undefined" &&
    typeof navigator.clipboard?.write === "function" &&
    (!ClipboardItem.supports || ClipboardItem.supports("image/png"))
  );
}

/** Export decoded pixels, never a blob URL or the current zoomed/cropped viewport. */
export function imagePng(image: HTMLImageElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    if (!image.naturalWidth || !image.naturalHeight) {
      reject(new Error("Image is not decoded"));
      return;
    }
    // Bound the extra RGBA allocation for unusually large decoded attachments.
    if (image.naturalWidth * image.naturalHeight > 32_000_000) {
      reject(new Error("Image is too large to copy; save it instead"));
      return;
    }
    const canvas = document.createElement("canvas");
    canvas.width = image.naturalWidth;
    canvas.height = image.naturalHeight;
    const context = canvas.getContext("2d");
    if (!context) {
      reject(new Error("Image export unavailable"));
      return;
    }
    context.drawImage(image, 0, 0);
    canvas.toBlob(
      (blob) =>
        blob ? resolve(blob) : reject(new Error("Image export failed")),
      "image/png",
    );
  });
}

/** Call write synchronously within the user's gesture: WebKit rejects writes after await. */
export async function copyImage(
  image: HTMLImageElement,
  confirmMessage: string,
  current: () => boolean,
): Promise<boolean> {
  if (!current()) return false;
  const policy = useSettings.getState().privacy;
  if (policy.confirm_sensitive_copy && !window.confirm(confirmMessage))
    return false;
  if (!canCopyImage()) throw new Error("Image clipboard is unavailable");
  const png = imagePng(image).then((blob) => {
    if (!current()) throw new Error("Image session has retired");
    return blob;
  });
  // A denied write may reject before the engine consumes the PNG promise.
  void png.catch(() => {});
  await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
  if (policy.clipboard_clear_seconds > 0) {
    const original = await png;
    window.setTimeout(() => {
      void clearImageIfUnchanged(original);
    }, policy.clipboard_clear_seconds * 1000);
  }
  return true;
}

async function clearImageIfUnchanged(original: Blob): Promise<void> {
  try {
    const items = await navigator.clipboard.read();
    if (items.length !== 1 || !items[0].types.includes("image/png")) return;
    const candidate = await items[0].getType("image/png");
    if (candidate.size !== original.size) return;
    const [a, b] = await Promise.all([
      original.arrayBuffer(),
      candidate.arrayBuffer(),
    ]);
    const bytes = new Uint8Array(b);
    if (new Uint8Array(a).every((byte, index) => byte === bytes[index]))
      await navigator.clipboard.writeText("");
  } catch {
    // No permission or a transcoded/new clipboard value: never overwrite unknown content.
  }
}
