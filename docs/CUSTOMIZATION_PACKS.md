# Customization packs

Mesh-Talk includes basic light, dark, and OLED themes and starts without optional packs. Avatar libraries, personal themes, and sticker libraries can be installed from the marketplace or a local ZIP file. Installed packs are stored per device in IndexedDB and remain available offline, including packs installed by earlier releases. Removing an avatar library does not change avatars already selected for a profile or channel, because selected avatars are copied into the existing avatar store. Removing the active theme switches the app to dark. Removing a sticker pack removes its collection from the picker. New sticker messages retain their own encrypted media independently; older ID-only messages display their fallback text when their pack is absent. Previously sent cat stickers use their original IDs and resolve to Cat Stickers artwork when that pack is installed. Previously sent unprefixed Noto stickers resolve through Google Noto (or a still-installed Noto Favorites pack); removing both leaves their emoji fallback. Existing Noto Favorites installations remain until the user removes them.

## Desktop builds

`cd frontend && npm run build` makes the frontend without optional pack ZIPs or a separate Noto artwork directory. Tauri builds use the same behavior. The release workflow publishes one desktop package per platform and CPU architecture, with no edition flag or filename suffix. Upgrading preserves packs already installed on the device.

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

Theme colors must use token names from the [shared format validator](https://github.com/OctopusGarage/marketplace/blob/main/src/lib/pack.ts) (for example `background`, `foreground`, `primary`, `card`, and `border`). At least one color is required. HSL hue must be 0–360; saturation and lightness must each be 0–100%. Wallpaper IDs must be unique within the pack and contain only lowercase letters, digits, and hyphens; wallpaper titles are at most 80 characters. A good workflow is to start with three colors, install the ZIP locally, and inspect both text and controls in the app before adding more overrides.

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

A sticker pack has 1–100 stickers. Each sticker ID is unique within its pack, uses lowercase letters, digits, underscores, or hyphens, and is at most 40 characters. `fallback` is required, at most eight Unicode code points, and appears when a recipient has not installed the same pack. New sends snapshot the selected sticker image and deliver it as encrypted media with a pack/sticker identity and fallback. Recipients do not need to install the pack to view the image. Older ID-only messages still depend on an installed matching pack. Google Noto is a removable 40-sticker pack available from the marketplace. Older unprefixed Noto messages retain emoji fallbacks when the pack is removed. Noto Favorites has been retired from the active catalog; existing installations and messages remain usable.

Package the files with a normal ZIP tool, keeping `manifest.json` at the root. Open **Settings → Appearance → Install ZIP** for a theme, open an avatar gallery and choose **Install ZIP** for an avatar library, or open a chat's **Stickers → Manage sticker packs → Install ZIP** for stickers. The same screens show marketplace packs with their versions and **Install**, **Reinstall**, or **Replace** actions, plus **Remove** for installed packs. Invalid packs leave installed packs untouched.

### Build and check your ZIP

The [Pack Studio](https://octopusgarage.github.io/marketplace/studio/) on GitHub Pages can build a ZIP from a source folder, verify an existing ZIP, show a small image preview and checksum, and download a creator toolkit with working avatar, theme, and sticker examples plus the command-line checker. It processes files locally in the browser; no pack is uploaded. The builder lists how many unrelated source files it omitted, and the latest selected ZIP or folder owns the result when checks overlap. The Studio is published by the shared marketplace repository after its PR is merged.

Use any image editor or drawing tool to create the images, then save them in the supported format. Clone [OctopusGarage/marketplace](https://github.com/OctopusGarage/marketplace), install its dependencies with `npm ci`, then run:

```bash
npm run pack:build -- ../my-pack ../my-pack.zip
npm run pack:check -- ../my-pack.zip
```

`pack:build` includes only images named in the manifest and an optional `credits.json`, refuses symlinks, and validates the generated ZIP. `pack:check` runs the app's ZIP parser and prints the pack type, image count, size, and SHA-256. To inspect an existing archive, use `npm run pack:check -- site/packs/<id>/<version>/<sha256>.zip`. You can also create a ZIP with a normal archive tool, but `manifest.json` must be at its root, not inside an extra folder.

### What installation checks

The app rejects oversized archives, duplicate ZIP entries, unexpected files, unsafe paths, unsupported image types, bad image signatures, invalid manifest values, and images that its browser engine cannot decode or that exceed 4096 pixels on either side or 16 million pixels total. Marketplace downloads also need to match the catalog's SHA-256 and the selected listing's ID, name, version, type, and avatar category. These checks protect the app's pack format; a checksum proves download integrity relative to the catalog, not that a creator is trustworthy. They cannot guarantee that an image has no maliciously crafted decoder payload, and they do not identify offensive, copyrighted, or otherwise inappropriate artwork. Review previews and the source before importing an unfamiliar pack. A locally installed ZIP does not become public automatically.

## Sticker message delivery

Installing a sticker pack adds a collection to the picker. Receiving one sticker
does not install or fetch the rest of its pack. New sends snapshot the selected
image and use ordinary encrypted media delivery; the attachment checksum binds
the exact bytes. Updating, replacing or uninstalling a pack therefore cannot
change retained new-message artwork. Legacy ID-only messages keep their existing
installed-pack lookup and fallback behavior; their original images cannot be
reconstructed reliably from an ID alone.

The deployed MFM3 media envelope remains unchanged. Its filename carries a
versioned presentation marker:
`mt-sticker-v1-<base64url-no-padding(UTF-8(id + newline + fallback))>.<extension>`.
The ID is 1–126 lowercase ASCII letters, digits, colons, periods, underscores or
hyphens. The fallback is 1–8 non-control Unicode scalar values, at most 32 UTF-8
bytes. The canonical filename is at most 233 ASCII bytes; supported extensions
are `png`, `jpg`, `webp` and `gif`, with matching MIME types. This marker conveys
presentation and source identity, never read authorization. Older media-capable
clients show the same attachment as an ordinary image. No new positional fields,
event kinds or file-kind discriminants are introduced.

Sticker payloads must be nonempty and at most 3 MiB, have at most 4096 pixels per
side and 16 million canvas pixels, and decode as their declared format. Animations
are limited to 256 frames and 64 million aggregate canvas pixels. A conservative
four-buffer canvas budget limits animation working buffers to 64 MiB; decoder
allocation limits are additional best-effort checks, not a process memory sandbox.
Complete bytes are validated before durable preview persistence or exposure.

Repeated sends can reuse an own-authored, retained, byte-identical sticker
transfer among the last 256 visible messages in the same conversation with the
same expiration deadline. Reuse requires complete verified source chunks;
missing, erased or incompatible sources stage a fresh transfer. File keys are
never reused across recipient conversations, groups or retention scopes. Normal
event reconciliation transfers only missing ciphertext chunks. This is scoped
reuse, not global hash-based discovery or convergent encryption.

Every repeated message still needs completion for its exact manifest event.
After ciphertext reclamation, a cached sticker may complete a new compatible
manifest only when retained verified ancestry, source identity/certificate,
recipient account, conversation and immutable transfer metadata agree and the
durable bytes pass their checksum and image validation. A cache path or matching
hash alone cannot create a delivery proof. This preserves exact completion
controls for relay-only delivery as well as direct historical custody checks.

Media retention, authorized source recovery, post-office custody, backup and
linked-device attachment policy belong to the existing file subsystem. Deleting
one message does not remove an image still referenced by another retained
message. Explicit cleanup, expiry and recall keep their existing authority.
A received message or past custody count does not guarantee available image
bytes: an authorized reachable source must still retain them. The interface
shows the fallback and transfer state while missing media is retried. Protected
sending remains unavailable for stickers.

## Account-scoped startup appearance

Installed private packs are cached under both the local user and the verified
cryptographic account. Startup restores the selected theme from that cache before
enumerating unrelated gallery assets. This does not require a network download or
completion of account synchronization.

The desktop can expose public account/device IDs after its existing keystore
unlock, while durable stores are still opening. That observation is bound to the
current desktop session and startup attempt; it does not grant messaging or
runtime access. Private appearance cannot load from an unverified last-account
identifier. A temporary readiness failure retains the same account's restored
appearance. Logout, account adoption or session replacement revokes the old
scope and its wallpaper CSS, including delayed cache reads.

The application root owns authenticated runtime and appearance startup before the
chat view is downloaded. Its startup canvas uses the same wallpaper composition
as chat and renders private images only for the matching unlocked user, account
and runtime session.

Normal native launches keep the initial window hidden until that authorized
surface and selected local wallpaper are ready for display. The pack owner
records when the selected-theme lookup has settled for the current scope; this
permits display while unrelated gallery enumeration continues. Storage without
that lookup retains full-library completion as a fallback.
Signed-out and locked launches prepare their public entry surfaces. A one-shot
native fallback preserves startup liveness; hidden autostart and explicit user
visibility choices remain authoritative. Full message-store readiness and remote
account synchronization do not delay a locally prepared appearance.

## Marketplace publishing

[OctopusGarage/marketplace](https://github.com/OctopusGarage/marketplace) owns the shared pack specification, validator, creator Studio, source artwork, catalog, and immutable versioned ZIPs. Its GitHub Pages site hosts the public catalog at `https://octopusgarage.github.io/marketplace/catalog.json`. A listing gives the app ID, version, type, supported apps, ZIP size and SHA-256. Mesh Talk checks the catalog path, size, hash, and manifest identity before installing. The catalog currently lists 15 packs; artwork rights review status is marked `pending` for migrated packs and must be checked before reuse elsewhere.

Mesh Talk keeps a snapshot of the 15 ZIPs in `site/market/packs/` for offline tests. Desktop builds include none. This snapshot is not served by Mesh Talk Pages; update it from a reviewed marketplace release when updating the test snapshot. The two repositories have separate PRs and release schedules. Publish the marketplace PR before a Mesh Talk release that relies on its catalog.

## Share a pack

To share privately, send your ZIP to another Mesh-Talk user; they can install it locally. For public discovery, open the [Share a customization pack issue form](https://github.com/OctopusGarage/marketplace/issues/new?template=pack_submission.yml) with a preview, a link to your source repository, the ZIP's ID/version and validation output, and image rights/attribution. Issues are for proposals and review; opening one does not publish a pack to the marketplace. For a curated listing, submit a pull request to the marketplace repository with source artwork and a generated catalog entry. Maintainers review technical safety, licensing, attribution, and content before merging; Pages publishes it only after a merge to `main`. The marketplace issue form is the supported public proposal path.
