# Customization packs

Mesh-Talk keeps its basic light, dark, and OLED themes and its built-in Noto stickers in both desktop editions. Avatar libraries, personal themes, and custom sticker libraries are ZIP packs. The default edition bundles and installs the eleven current avatar and theme collections on first launch. Each is removable; removing one does not cause it to return on restart. The lite edition bundles no avatar or personal theme packs and starts with an empty optional library. Both editions can install a pack from the marketplace or a local ZIP file. Installed packs are stored per device in IndexedDB and remain available offline. Switching editions preserves packs already installed on that device. Removing an avatar library does not change avatars already selected for a profile or channel, because selected avatars are copied into the existing avatar store. Removing the active theme switches the app to dark. Removing a sticker pack makes its previously sent stickers display their fallback text on this device.

## Build editions

`cd frontend && npm run build` makes the default frontend and includes eleven ZIPs under `dist/builtin-packs/`. `npm run build:lite` makes the lite frontend, with no `builtin-packs/` directory. To build a lite desktop installer, set `MESH_TALK_VARIANT=lite` in the environment before running the Tauri build; the release workflow builds both editions for each platform. Default release ZIP names are unchanged, while lite release ZIPs end in `_lite.zip`. The editions use the same app identity, so they replace one another rather than install side by side.

## Create a pack

Place `manifest.json` at the ZIP root and image files under `images/`. The ZIP must be at most 12 MiB, contain at most 256 entries, have at most 24 MiB of extracted content, and use images of at most 3 MiB each. Avatar and theme images must be PNG, JPEG, or WebP; sticker images may also be GIF. Animated WebP, APNG, and GIF stickers work in browsers that support those formats. SVG, CSS, JavaScript, remote image URLs, executables, and unrelated files are not accepted. `credits.json` is allowed for attribution. Names and IDs are plain text. Pack IDs must be 3–80 lowercase letters, digits, dots, underscores, or hyphens. Use a stable ID and increment `version` in `major.minor.patch` form when publishing an update; installing a pack with the same ID and library type replaces it. An avatar pack cannot replace a pack with the same ID in another avatar category.

Create a source folder with this layout (replace the example images with your own):

```text
my-pack/
├── manifest.json
└── images/
    ├── first.webp
    └── second.png
```

The `format` value is always `1`. `id` must be unique across all pack types; avoid the reserved IDs `light`, `dark`, and `oled`. `name` and each image label may be at most 80 characters. Every referenced image must be inside `images/`, with no `.` or `..` path segments. A ZIP may contain up to 100 avatars or stickers, or 60 theme wallpapers. A ZIP created on macOS should exclude `.DS_Store` and `__MACOSX/`.

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

`base` may be `light` or `dark`. `colors` overrides the named design tokens from `frontend/src/lib/pack.ts`; each value is an HSL triplet without `hsl(...)`. A theme may also provide a `crest` image, or a `wallpapers` array of `{ "id", "title", "file" }` entries instead of one `wallpaper`. The app remembers the selected wallpaper separately for each installed theme and shows it in that theme's preview. Keep text and controls legible against your colors; the app does not automatically correct contrast.

Theme colors must use token names from [`frontend/src/lib/pack.ts`](../frontend/src/lib/pack.ts) (for example `background`, `foreground`, `primary`, `card`, and `border`). At least one color is required. HSL hue must be 0–360; saturation and lightness must each be 0–100%. Wallpaper IDs must be unique within the pack and contain only lowercase letters, digits, and hyphens; wallpaper titles are at most 80 characters. A good workflow is to start with three colors, install the ZIP locally, and inspect both text and controls in the app before adding more overrides.

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

Package the files with a normal ZIP tool, keeping `manifest.json` at the root. Open **Settings → Appearance → Install ZIP** for a theme, open an avatar gallery and choose **Install ZIP** for an avatar library, or open a chat's **Stickers → Manage sticker packs → Install ZIP** for stickers. The same screens show marketplace packs with their versions and **Install**, **Reinstall**, or **Replace** actions, plus **Remove** for installed packs. Invalid packs leave installed packs untouched.

### Build and check your ZIP

The [Pack Studio](https://octopusgarage.github.io/mesh-talk/market/studio/) on GitHub Pages can build a ZIP from a source folder, verify an existing ZIP, show a small image preview and checksum, and download a creator toolkit with working avatar, theme, and sticker examples plus the command-line checker. It processes files locally in the browser; no pack is uploaded. The builder lists how many unrelated source files it omitted, and the latest selected ZIP or folder owns the result when checks overlap. The Studio is part of this draft PR and will become available on Pages after publication.

Use any image editor or drawing tool to create the images, then save them in the supported format. From the repository root, install the frontend dependencies once with `cd frontend && npm ci`. With Node 22.6 or newer, run:

```bash
cd frontend
npm run pack:build -- ../my-pack ../my-pack.zip
npm run pack:check -- ../my-pack.zip
```

`pack:build` includes only images named in the manifest and an optional `credits.json`, refuses symlinks, and validates the generated ZIP. `pack:check` runs the app's ZIP parser and prints the pack type, image count, size, and SHA-256. To inspect an existing archive, use `npm run pack:check -- ../site/market/packs/players.zip`. You can also create a ZIP with a normal archive tool, but `manifest.json` must be at its root, not inside an extra folder.

### What installation checks

The app rejects oversized archives, duplicate ZIP entries, unexpected files, unsafe paths, unsupported image types, bad image signatures, invalid manifest values, and images that its browser engine cannot decode or that exceed 4096 pixels on either side or 16 million pixels total. Marketplace downloads also need to match the catalog's SHA-256 and the selected listing's ID, name, version, type, and avatar category. These checks protect the app's pack format; a checksum proves download integrity relative to the catalog, not that a creator is trustworthy. They cannot guarantee that an image has no maliciously crafted decoder payload, and they do not identify offensive, copyrighted, or otherwise inappropriate artwork. Review previews and the source before importing an unfamiliar pack. A locally installed ZIP does not become public automatically.

## Marketplace publishing

The public market is `site/market/`, deployed by `.github/workflows/pages.yml` with the rest of the site on pushes to `main`. Until this feature branch is merged and Pages deploys, its packs are available from the branch's `site/market/packs/` directory on GitHub; the in-app catalog points to Pages and will not list branch-only packs. GitHub Releases distributes the desktop installers, not the individual customization packs. `site/market/catalog.json` lists ZIPs and SHA-256 hashes; the app checks the hash before installing a download. The source artwork for the avatar/theme catalog is in `marketplace/assets/`; Noto Favorites uses the existing Noto sticker assets. Run `python3 scripts/build-market.py` after changing artwork or source theme palettes in `frontend/src/index.css`, then commit the generated catalog and ZIPs. The script requires ImageMagick (`magick`) to convert bundled SVG logos to inert PNGs. To add another curated pack, add its source artwork and an entry to the script's `AVATARS`, `THEMES`, or sticker section, then regenerate the market.

## Share a pack

To share privately, send your ZIP to another Mesh-Talk user; they can install it locally. For public discovery, open the [Share a customization pack issue form](https://github.com/OctopusGarage/mesh-talk/issues/new?template=pack_submission.yml) with a preview, a link to your source repository, the ZIP's ID/version and validation output, and image rights/attribution. Issues are for proposals and review; opening one does not publish a pack to the marketplace. For a curated listing, submit a pull request adding source artwork, the generated ZIP, and its catalog entry. Maintainers review technical safety, licensing, attribution, and content before merging; Pages publishes it only after a merge to `main`. GitHub Discussions is currently disabled for this repository, so the issue form is the supported public conversation path.
