import { initialPresentationReady } from "./api/window";

interface PresentationPorts {
  decode: (url: string) => Promise<void>;
  commit: () => Promise<void>;
  acknowledge: () => Promise<void>;
}

async function decodeWallpaper(url: string): Promise<void> {
  const image = new Image();
  image.src = url;
  await image.decode();
}

async function commitPresentation(): Promise<void> {
  // Hidden native windows can suspend requestAnimationFrame indefinitely.
  // A timer turn followed by layout works before WKWebView is first mapped.
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  document.documentElement.getBoundingClientRect();
}

/** Decode only the already-authorized theme; never fetch or infer an account. */
export async function prepareInitialPresentation(
  wallpaper: string | null,
  current: () => boolean,
  ports: Partial<PresentationPorts> = {},
): Promise<void> {
  const {
    decode = decodeWallpaper,
    commit = commitPresentation,
    acknowledge = initialPresentationReady,
  } = ports;
  if (!current()) return;
  if (wallpaper) await decode(wallpaper);
  if (!current()) return;
  await commit();
  if (current()) await acknowledge();
}
