# OneShell Brand Assets

Source files for the OneShell logo. The 1024 × 1024 master is the
single source of truth; every PNG / `.icns` / `.ico` in
`src-tauri/icons/` is generated from it.

## Files

| File | Use |
|---|---|
| [`icon.svg`](./icon.svg) | Master app icon (1024×1024 viewBox). Re-render at any size. |
| [`icon-horizontal.svg`](./icon-horizontal.svg) | README / docs lockup. Uses `prefers-color-scheme` to swap wordmark + cursor colors for light/dark pages. |
| `../../../docs/assets/icon.png` | 1024 × 1024 raster — bundled, `store` assets, social previews. |
| `../../../docs/assets/logo-horizontal-light.png` | Rasterized horizontal lockup for light-themed pages. |
| `../../../docs/assets/logo-horizontal-dark.png` | Rasterized horizontal lockup for dark-themed pages. |
| [`../../../src-tauri/icons/`](../../../src-tauri/icons/) | Tauri-required PNG / `.icns` / `.ico` set, generated from the master. |

## Brand

- **Mint green** `#00FF87` — primary accent, terminal frame, `$` prompt, cursor.
- **Surface** `#0A0F0C` → `#0F1A14` — vertical gradient (darker at the bottom).
- **Wordmark light** `#E6F7EE` (dark surfaces).
- **Wordmark dark**  `#0A0F0C` (light surfaces).
- **Cursor accent (light bg)** `#00B86B` — a slightly desaturated mint so the
  cursor doesn't burn the eye on a white README.
- **Traffic lights** `#FF5F57` / `#FEBC2E` / `#28C840` at 0.8 opacity (the
  green dot stays at full saturation — it's the only one that matters).

## Concept

A single terminal window holds the entire brand. The rounded square
*is* the app icon; the inner frame is the terminal; the `$` and the
block cursor tell you what kind of tool this is before you read a word.
The 3 traffic-light dots anchor the metaphor (macOS / Linux desktop)
without leaning on any one platform's visual language.

## Regenerating PNGs

```bash
# from project root — master 1024×1024
magick src/assets/logo/icon.svg -density 300 -resize 1024x1024 docs/assets/icon.png
# then derive every other size
sips -z 128 128  docs/assets/icon.png --out src-tauri/icons/128x128.png
# ... and so on for 32, 256, Square* sizes

# .icns (macOS) — needs the .iconset dance
mkdir /tmp/OneShell.iconset
for pair in "16" "32" "32:32" "64" "128" "256" "256" "512" "1024"; do :; done  # see iconutil docs
iconutil -c icns /tmp/OneShell.iconset --output src-tauri/icons/icon.icns

# .ico (Windows)
magick docs/assets/icon.png -define icon:auto-resize=256,128,64,48,32,16 \
  src-tauri/icons/icon.ico
```

In practice, re-render via Chrome canvas (see git history) — it handles
the `ui-monospace` font stack correctly; ImageMagick's freetype path
doesn't.
