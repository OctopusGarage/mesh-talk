#!/usr/bin/env python3
"""Build the static GitHub Pages customization catalog from marketplace assets."""
import hashlib
import io
import json
import re
import subprocess
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "marketplace/assets"
OUT = ROOT / "site/market"
PACKS = OUT / "packs"
PACKS.mkdir(parents=True, exist_ok=True)
CSS = (ROOT / "frontend/src/index.css").read_text()
CATALOG = []


def label(path):
    name = re.sub(r"^\d+-", "", path.stem)
    return name if " " in name else name.replace("-", " ").title()


def add_image(files, path, destination=None):
    destination = destination or f"images/{hashlib.sha256(str(path.relative_to(ASSETS)).encode()).hexdigest()[:16]}{path.suffix.lower()}"
    if path.suffix.lower() == ".svg":
        destination = str(Path(destination).with_suffix(".png"))
        files[destination] = subprocess.check_output(
            ["magick", str(path), "-background", "none", "-resize", "256x256", "png:-"]
        )
    else:
        content = path.read_bytes()
        if content.startswith(b"RIFF") and content[8:12] == b"WEBP":
            destination = str(Path(destination).with_suffix(".webp"))
        files[destination] = content
    return destination


def publish(manifest, files, description):
    archive = PACKS / f"{manifest['id']}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as output:
        for name, content in sorted({"manifest.json": json.dumps(manifest, ensure_ascii=False, separators=(",", ":")).encode(), **files}.items()):
            info = zipfile.ZipInfo(name, date_time=(2024, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            output.writestr(info, content, compress_type=zipfile.ZIP_DEFLATED, compresslevel=6)
    CATALOG.append({
        "id": manifest["id"], "name": manifest["name"], "kind": manifest["kind"],
        **({"category": manifest["category"]} if manifest["kind"] == "avatar" else {}),
        "description": description, "file": f"packs/{archive.name}",
        "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
    })


AVATARS = [
    ("players", "Football stars", "personal", "cover", "Football player portraits"),
    ("nba-players", "NBA stars", "personal", "cover", "Basketball player portraits"),
    ("sports", "Sports stars", "personal", "cover", "Athletes from different sports"),
    ("famous", "Famous people", "personal", "cover", "Portraits of notable people"),
    ("cities", "City paintings", "group", "contain", "Painted city landmarks"),
    ("clubs", "Football clubs", "group", "contain", "Football club emblems"),
    ("nba-teams", "NBA teams", "group", "contain", "Basketball team emblems"),
]
for pack_id, name, category, fit, description in AVATARS:
    source = ASSETS / "avatars" / pack_id
    files = {}
    entries = []
    for path in sorted(source.iterdir()):
        if path.suffix.lower() not in {".png", ".jpg", ".jpeg", ".webp", ".svg"}:
            continue
        entries.append({"label": label(path), "file": add_image(files, path)})
    if pack_id == "cities":
        files["credits.json"] = (source / "credits.json").read_bytes()
    publish({"format": 1, "id": pack_id, "version": "1.0.0", "name": name,
             "kind": "avatar", "category": category, "fit": fit, "avatars": entries}, files, description)


def palette(name):
    match = re.search(r'html\[data-palette="' + name + r'"\] \{([^}]*)\}', CSS)
    if not match:
        raise ValueError(f"Missing {name} palette")
    return {key: value.strip() for key, value in re.findall(r"--([a-z-]+):\s*([^;]+);", match.group(1))
            if key not in {"radius"}}


THEMES = [
    ("argentina", "Argentina", "light", "Sky blue and white with a stadium wallpaper"),
    ("barcelona", "Barcelona", "dark", "Deep blue and garnet with a stadium wallpaper"),
    ("messi", "Messi", "light", "Bright blue palette with a football wallpaper"),
    ("nature", "Nature", "light", "Forest green palette and fifty landscape wallpapers"),
]
for pack_id, name, base, description in THEMES:
    files = {}
    manifest = {"format": 1, "id": pack_id, "version": "1.0.0", "name": name,
                "kind": "theme", "base": base, "colors": palette(pack_id)}
    if pack_id == "nature":
        manifest["wallpapers"] = [
            {"id": path.stem, "title": label(path).replace(" Wallpaper", "").replace("Giant S", "Giant's"),
             "file": add_image(files, path)}
            for path in sorted((ASSETS / "themes/nature").glob("*.webp"))
        ]
    else:
        background = next((ASSETS / "themes").glob(f"{pack_id}-bg.*"))
        manifest["wallpaper"] = add_image(files, background)
        crest = next((ASSETS / "themes").glob(f"{pack_id}-crest.*"))
        manifest["crest"] = add_image(files, crest)
    publish(manifest, files, description)

STICKER_SOURCE = ROOT / "frontend/src/assets/stickers/noto"
STICKERS = [
    ("1f44d", "Thumbs up"), ("1f602", "Joy"), ("1f60d", "Heart eyes"),
    ("1f622", "Crying"), ("1f64f", "Thank you"), ("1f680", "Rocket"),
    ("1f389", "Celebration"), ("2764_fe0f", "Heart"),
]
sticker_files = {}
sticker_entries = []
for sticker_id, sticker_label in STICKERS:
    source = STICKER_SOURCE / f"{sticker_id}.webp"
    destination = f"images/{sticker_id}.webp"
    sticker_files[destination] = source.read_bytes()
    sticker_entries.append({
        "id": sticker_id,
        "label": sticker_label,
        "fallback": "".join(chr(int(codepoint, 16)) for codepoint in sticker_id.split("_")),
        "file": destination,
    })
publish({"format": 1, "id": "noto-favorites", "version": "1.0.0",
         "name": "Noto Favorites", "kind": "sticker", "stickers": sticker_entries},
        sticker_files, "Eight animated emoji stickers from Google Noto")

(OUT / "catalog.json").write_text(json.dumps(CATALOG, ensure_ascii=False, indent=2) + "\n")
print(f"Built {len(CATALOG)} packs in {OUT}")
