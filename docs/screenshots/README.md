# Desktop screenshots

These images show the current React interface against deterministic demo data.
`app-conversations` and `app-verification` use the default dark appearance; the
three `hero-*` captures show optional Barcelona, Argentina and Messi themes.
They are UI examples, not captures of a real network or evidence of message delivery.

The capture renders a 1280 × 800 CSS-pixel viewport at native 2× pixel density,
producing 2560 × 1600 PNGs. This renders text and controls at the target density;
upscaling an existing screenshot does not provide equivalent detail.
The website uses lossless WebP at 1280 and 2560 pixels through responsive image
candidates. Enlarged views and ordinary image links use the full-resolution file.
The root README reuses the website assets and links to full-resolution files.
`docs/screenshots/` also retains the 1280-pixel themed and settings exports for
other documentation embeds.

To refresh them after a frontend visual change (requires `cwebp`):

```bash
cd frontend
MESH_TALK_CAPTURE_SITE=1 npm run e2e -- --reporter=line e2e/site-capture.spec.ts
cd ..
for capture_name in app-conversations app-verification hero-barcelona hero-argentina hero-messi verify settings themes stickers avatars-players avatars-clubs; do
  capture_png="tmp/site-captures/$capture_name.png"
  cwebp -quiet -lossless -m 6 "$capture_png" -o "site/screenshots/$capture_name@2x.webp"
  cwebp -quiet -lossless -m 6 -resize 1280 800 "$capture_png" -o "site/screenshots/$capture_name.webp"
  case "$capture_name" in
    app-*) ;;
    *) cp "site/screenshots/$capture_name.webp" "docs/screenshots/$capture_name.webp" ;;
  esac
done
```

The maintained capture uses mocked Tauri IPC, waits for message and avatar content,
and uses reduced motion for stable frames. It asserts the native PNG dimensions.
Review every image before committing it, and keep descriptions accurate. Public
browser tests check Retina image selection, mobile candidates, decoded theme
switches and full-resolution viewer links.
