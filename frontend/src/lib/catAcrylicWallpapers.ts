const images = import.meta.glob<string>("../assets/themes/cat-acrylic/*.webp", {
  eager: true,
  query: "?url",
  import: "default",
});

export interface CatAcrylicWallpaper {
  id: string;
  title: string;
  url: string;
}

export const CAT_ACRYLIC_WALLPAPERS: CatAcrylicWallpaper[] = Object.entries(
  images,
)
  .sort(([a], [b]) => a.localeCompare(b))
  .map(([path, url]) => {
    const id = path
      .split("/")
      .pop()!
      .replace(/\.webp$/, "");
    return { id, title: id.replace("cat-", "Cat "), url };
  });

export const DEFAULT_CAT_ACRYLIC_WALLPAPER = CAT_ACRYLIC_WALLPAPERS[0];

export function getCatAcrylicWallpaper(id: string): CatAcrylicWallpaper {
  return (
    CAT_ACRYLIC_WALLPAPERS.find((wallpaper) => wallpaper.id === id) ??
    DEFAULT_CAT_ACRYLIC_WALLPAPER
  );
}
