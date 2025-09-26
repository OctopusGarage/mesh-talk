# Mesh-Talk Icons

This directory contains all the icon assets for the Mesh-Talk desktop application.

## Icon Source

The main icon design is located at `../icons-src/mesh-talk-icon.svg` and features:
- A purple (#4f46e5) background representing the mesh network
- Three interconnected white circles representing nodes in the network
- "MESH" text at the bottom to clearly identify the application
- Modern, clean design suitable for all platforms

## Generated Icons

The following formats and sizes have been generated:

### Windows
- `icon.ico` - Multi-resolution ICO file with 16, 32, 48, and 256px sizes
- `icon.png` - Main 512x512px icon (also used as default)

### macOS
- `icon.icns` - Multi-resolution ICNS file with various sizes (16x16 to 512x512)

### Android (for Tauri Android builds)
- Various densities in `android/mipmap-*` directories:
  - mdpi (48x48)
  - hdpi (72x72)
  - xhdpi (96x96)
  - xxhdpi (144x144)
  - xxxhdpi (192x192)

### iOS (for Tauri iOS builds)
- Various sizes from 20x20 to 512x512 in the `ios/` directory

## Usage

These icons are automatically used by Tauri during the build process according to the configuration in `tauri.conf.json`.

## Updating Icons

To update the icons:
1. Modify the source SVG at `../icons-src/mesh-talk-icon.svg`
2. Regenerate the various formats using ImageMagick:
   ```bash
   # Example for a 128x128 icon
   magick -background none ../icons-src/mesh-talk-icon.svg -resize 128x128 icon-128x128.png
   ```