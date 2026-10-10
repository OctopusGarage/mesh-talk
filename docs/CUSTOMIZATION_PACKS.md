# Customization packs

Mesh-Talk keeps its basic light, dark, and OLED themes and its built-in Noto stickers in both desktop editions. Avatar libraries, personal themes, and custom sticker libraries are ZIP packs. The default edition bundles and installs the eleven current avatar and theme collections on first launch. Each is removable; removing one does not cause it to return on restart. The lite edition bundles no avatar or personal theme packs and starts with an empty optional library. Both editions can install a pack from the marketplace or a local ZIP file. Installed packs are stored per device in IndexedDB and remain available offline. Switching editions preserves packs already installed on that device. Removing an avatar library does not change avatars already selected for a profile or channel, because selected avatars are copied into the existing avatar store. Removing the active theme switches the app to dark. Removing a sticker pack makes its previously sent stickers display their fallback text on this device.

## Build editions

`cd frontend && npm run build` makes the default frontend and includes eleven ZIPs under `dist/builtin-packs/`. `npm run build:lite` makes the lite frontend, with no `builtin-packs/` directory. To build a lite desktop installer, set `MESH_TALK_VARIANT=lite` in the environment before running the Tauri build; the release workflow builds both editions for each platform. Default release ZIP names are unchanged, while lite release ZIPs end in `_lite.zip`. The editions use the same app identity, so they replace one another rather than install side by side.

## Create a pack

Place `manifest.json` at the ZIP root and image files under `images/`. The ZIP must be at most 12 MiB, its extracted content at most 24 MiB, and each image at most 3 MiB. Avatar and theme images must be PNG, JPEG, or WebP; sticker images may also be GIF. Animated WebP, APNG, and GIF stickers work in browsers that support those formats. SVG, CSS, JavaScript, and remote image URLs are not accepted. Names and IDs are plain text. Pack IDs must be 3–80 lowercase letters, digits, dots, underscores, or hyphens. Use a stable ID and increment `version` in `major.minor.patch` form when publishing an update; installing a pack with the same ID replaces it.

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

A sticker library:

```json
{
  "format": 1,
  "id": "example.reactions",
  "version": "1.0.0",
  "name": "Reactions",
  "kind": "sticker",
  "stickers": [
    { "id": "wave", "label": "Wave", "fallback": "👋", "file": "images/wave.webp" }
  ]
}
```

A sticker pack has 1–100 stickers. Each sticker ID is unique within its pack, uses lowercase letters, digits, underscores, or hyphens, and is at most 40 characters. `fallback` is required, at most eight Unicode code points, and appears when a recipient has not installed the same pack. Sticker images are stored locally; chat messages send only the pack/sticker ID and fallback, so recipients must install the pack to see its artwork. The built-in Noto stickers remain available in both editions. The marketplace also offers a downloadable Noto Favorites example pack; it is not preinstalled.

Package the files with a normal ZIP tool, keeping `manifest.json` at the root. Open **Settings → Appearance → Install ZIP** for a theme, open an avatar gallery and choose **Install ZIP** for an avatar library, or open a chat's **Stickers → Manage sticker packs → Install ZIP** for stickers. The same screens show marketplace packs with **Install** or **Update** and installed packs with **Remove**. Invalid packs leave installed packs untouched.

## Marketplace publishing

The public market is `site/market/`, deployed by `.github/workflows/pages.yml` with the rest of the site on pushes to `main`. Until this feature branch is merged and Pages deploys, its packs are available from the branch's `site/market/packs/` directory on GitHub; the in-app catalog points to Pages and will not list branch-only packs. GitHub Releases distributes the desktop installers, not the individual customization packs. `site/market/catalog.json` lists ZIPs and SHA-256 hashes; the app checks the hash before installing a download. The source artwork for the avatar/theme catalog is in `marketplace/assets/`; Noto Favorites uses the existing Noto sticker assets. Run `python3 scripts/build-market.py` after changing artwork or source theme palettes in `frontend/src/index.css`, then commit the generated catalog and ZIPs. The script requires ImageMagick (`magick`) to convert bundled SVG logos to inert PNGs. To add another curated pack, add its source artwork and an entry to the script's `AVATARS`, `THEMES`, or sticker section, then regenerate the market.
