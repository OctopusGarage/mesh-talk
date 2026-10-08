# Desktop screenshots

These images show the current v0.2.0 React interface against deterministic
football-themed demo data. The three `hero-*.webp` captures share the same
conversation and show the Barcelona, Argentina, and Messi palettes.
They are UI examples, not captures of a real network or evidence of message delivery.
The same WebP files are used by the root README and the static site.

To refresh them after a frontend visual change:

```bash
cd frontend
MESH_TALK_CAPTURE_SITE=1 npm run e2e -- --reporter=line e2e/site-capture.spec.ts
cd ..
for capture_name in hero-barcelona hero-argentina hero-messi verify settings themes stickers avatars-players avatars-clubs; do
  capture_png="tmp/site-captures/$capture_name.png"
  cwebp -quiet -q 84 "$capture_png" -o "site/screenshots/$capture_name.webp"
  cp "site/screenshots/$capture_name.webp" "docs/screenshots/$capture_name.webp"
done
```

The capture uses mocked Tauri IPC and reduced motion for stable frames. Review every
image before committing it, and keep the site and README descriptions accurate.
