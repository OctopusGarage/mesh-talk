/**
 * Browser-side avatar image pipeline: pick an image file, then crop/resize it down to a
 * small square via a canvas so the stored data-URL stays tiny (~10–30KB) — we never persist
 * the full original. Uses a plain `<input type="file">` (reliable in the Tauri webview)
 * rather than the native dialog + fs-read, since we only need the bytes in-page to draw to
 * canvas. The user chooses the crop region in `AvatarCropDialog`; `extractAvatar` below
 * turns that choice into the final data-URL.
 */

/** Target edge for the resized avatar. */
export const AVATAR_SIZE = 256;
/** JPEG quality for the resized output (keeps data-URLs small while staying crisp). */
export const AVATAR_QUALITY = 0.85;

/** Open a native file picker filtered to images; resolves to the chosen File or null. */
export function pickImageFile(accept = "image/*"): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.onchange = () => resolve(input.files?.[0] ?? null);
    // If the dialog is dismissed there is no `change`; we simply never resolve a file,
    // which is fine — the caller is awaiting a user action with no timeout.
    input.click();
  });
}

/** Load a File into an HTMLImageElement via an object URL (revoked once decoded). */
export function loadImage(file: File): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("Could not decode image"));
    };
    img.src = url;
  });
}

/** Load an image from a URL (for the built-in preset avatar packs). */
export function loadImageUrl(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Could not load preset image"));
    img.src = url;
  });
}

/**
 * Normalize a built-in preset (by URL) into the same 256×256 JPEG data-URL an upload
 * produces, so a chosen preset is stored + propagated exactly like a custom photo.
 * `contain` fits the whole image (logos keep their shape); `cover` fills the square,
 * cropping (player photos). SVGs that report no natural size fall back to the canvas size.
 */
export async function presetToAvatarDataUrl(
  url: string,
  fit: "cover" | "contain",
): Promise<string> {
  const img = await loadImageUrl(url);
  const canvas = document.createElement("canvas");
  canvas.width = AVATAR_SIZE;
  canvas.height = AVATAR_SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context");
  const iw = img.naturalWidth || AVATAR_SIZE;
  const ih = img.naturalHeight || AVATAR_SIZE;
  const scale =
    fit === "contain"
      ? Math.min(AVATAR_SIZE / iw, AVATAR_SIZE / ih)
      : Math.max(AVATAR_SIZE / iw, AVATAR_SIZE / ih);
  // JPEG has no alpha: any transparent pixels (a logo's padding, a cutout player photo's
  // background) would otherwise flatten to BLACK. Lay down a light field first so presets
  // get a clean, light backdrop instead.
  ctx.fillStyle = "#f1f3f5";
  ctx.fillRect(0, 0, AVATAR_SIZE, AVATAR_SIZE);
  const w = iw * scale;
  const h = ih * scale;
  ctx.drawImage(img, (AVATAR_SIZE - w) / 2, (AVATAR_SIZE - h) / 2, w, h);
  return canvas.toDataURL("image/jpeg", AVATAR_QUALITY);
}

/** A square crop region in the source image's natural pixel coordinates. */
export interface CropRegion {
  /** Left edge of the square, in source pixels. */
  x: number;
  /** Top edge of the square, in source pixels. */
  y: number;
  /** Side length of the square, in source pixels. */
  size: number;
}

/**
 * Draw the chosen square region of an image onto a fixed AVATAR_SIZE canvas and return a
 * JPEG data-URL. The region is already square (chosen via the crop UI), so scaling it to a
 * square target never distorts the image.
 */
export function extractAvatar(
  img: HTMLImageElement,
  region: CropRegion,
): string {
  const canvas = document.createElement("canvas");
  canvas.width = AVATAR_SIZE;
  canvas.height = AVATAR_SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas unavailable");
  ctx.drawImage(
    img,
    region.x,
    region.y,
    region.size,
    region.size,
    0,
    0,
    AVATAR_SIZE,
    AVATAR_SIZE,
  );
  return canvas.toDataURL("image/jpeg", AVATAR_QUALITY);
}
