export const IMAGE_INLINE_CAP = 16 * 1024 * 1024;
/** File-name media classification shared by pickers, previews, and history. */
const IMAGE_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  avif: "image/avif",
  svg: "image/svg+xml",
  heic: "image/heic",
  heif: "image/heif",
};
const VIDEO_MIME: Record<string, string> = {
  mp4: "video/mp4",
  mov: "video/quicktime",
  webm: "video/webm",
  m4v: "video/x-m4v",
  ogv: "video/ogg",
};

export const IMAGE_EXTENSIONS = Object.keys(IMAGE_MIME);
export const PROTECTED_IMAGE_EXTENSIONS = IMAGE_EXTENSIONS.filter(
  (extension) => extension !== "svg",
);
export const isProtectedImage = (name: string): boolean =>
  isImage(name) && !/\.svg$/i.test(name);

export const VIDEO_EXTENSIONS = Object.keys(VIDEO_MIME);

const IMAGE_EXT = new RegExp(`\\.(${IMAGE_EXTENSIONS.join("|")})$`, "i");
const VIDEO_EXT = new RegExp(`\\.(${VIDEO_EXTENSIONS.join("|")})$`, "i");

export const isImage = (name: string): boolean => IMAGE_EXT.test(name);
export const isVideo = (name: string): boolean => VIDEO_EXT.test(name);

export function mediaMimeFromName(name: string): string | undefined {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  return IMAGE_MIME[ext] ?? VIDEO_MIME[ext];
}
