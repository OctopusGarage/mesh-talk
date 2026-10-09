const images = import.meta.glob<string>("../assets/themes/nature/*.webp", {
  eager: true,
  query: "?url",
  import: "default",
});

export interface NatureWallpaper {
  id: string;
  title: string;
  url: string;
}

export const NATURE_WALLPAPERS: NatureWallpaper[] = Object.entries(images)
  .map(([path, url]) => {
    const id = path
      .split("/")
      .pop()!
      .replace(/\.webp$/, "");
    const title = id
      .replace(/^\d+-/, "")
      .replace(/-wallpaper$/, "")
      .replace(/-/g, " ")
      .replace(/\b\w/g, (letter) => letter.toUpperCase())
      .replace("Giant S Causeway", "Giant's Causeway");
    return { id, title, url };
  })
  .sort((a, b) => a.id.localeCompare(b.id));

export const DEFAULT_NATURE_WALLPAPER = NATURE_WALLPAPERS[0];

export function getNatureWallpaper(id: string): NatureWallpaper {
  return (
    NATURE_WALLPAPERS.find((wallpaper) => wallpaper.id === id) ??
    DEFAULT_NATURE_WALLPAPER
  );
}
