# Customization packs

Mesh-Talk keeps its basic light, dark, and OLED themes in the desktop app. Avatar libraries and personal themes are optional ZIP packs. A pack can be installed from the app's marketplace or from a local ZIP file. Installed packs are stored per device in IndexedDB; they are available offline after installation. Removing an avatar library does not change avatars already selected for a profile or channel, because selected avatars are copied into the existing avatar store. Removing the active theme switches the app to dark.

## Create a pack

Place `manifest.json` at the ZIP root and image files under `images/`. The ZIP must be at most 12 MiB, its extracted content at most 24 MiB, and each image at most 3 MiB. Images must be PNG, JPEG, or WebP; SVG, CSS, JavaScript, and remote image URLs are not accepted. Names and IDs are plain text. Pack IDs must be 3–80 lowercase letters, digits, dots, underscores, or hyphens. Use a stable ID and increment `version` in `major.minor.patch` form when publishing an update; installing a pack with the same ID replaces it.

An avatar library:

```json
{
  "format": 1,
  "id": "example.portraits",
  "version": "1.0.0",
  "name": "Portraits",
  "kind": "avatar",
  "category": "personal",
  "fit": "cover",
  "avatars": [
    { "label": "Ada", "file": "images/ada.webp" }
  ]
}
```

Set `category` to `group` for channel avatars. Set `fit` to `contain` for logos or artwork that should remain fully visible.

A theme:

```json
{
  "format": 1,
  "id": "example.forest",
  "version": "1.0.0",
  "name": "Forest",
  "kind": "theme",
  "base": "light",
  "colors": {
    "background": "44 34% 96%",
    "foreground": "160 22% 16%",
    "primary": "154 42% 31%"
  },
  "wallpaper": "images/forest.webp"
}
```

`base` may be `light` or `dark`. `colors` overrides the named design tokens from `frontend/src/lib/pack.ts`; each value is an HSL triplet without `hsl(...)`. A theme may also provide a `crest` image, or a `wallpapers` array of `{ "id", "title", "file" }` entries instead of one `wallpaper`. Keep text and controls legible against your colors; the app does not automatically correct contrast.

Package the files with a normal ZIP tool, keeping `manifest.json` at the root. Open **Settings → Appearance → Install ZIP** for a theme, or open an avatar gallery and choose **Install ZIP** for an avatar library. Invalid packs leave installed packs untouched.

## Marketplace publishing

The public market is `site/market/`, deployed by `.github/workflows/pages.yml` with the rest of the site on pushes to `main`. `site/market/catalog.json` lists ZIPs and SHA-256 hashes; the app checks the hash before installing a download. The source artwork for the built-in catalog is in `marketplace/assets/`. Run `python3 scripts/build-market.py` after changing that artwork or the source theme palettes in `frontend/src/index.css`, then commit the generated catalog and ZIPs. The script requires ImageMagick (`magick`) to convert bundled SVG logos to inert PNGs. To add another curated pack, add its source artwork and an entry to the script's `AVATARS` or `THEMES` list, then regenerate the market.
