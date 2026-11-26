# OpenMaker artwork

The logo combines an open folder built from diagonal-cut blocks with a
rust-orange cog. These files were traced and expanded from the approved
concept in ArtCraft's VectorCraft 0.23.0. The lettering is outlined; the
SVGs contain paths rather than an embedded image or a font dependency.

| File | Use |
|---|---|
| [openmaker-logo.svg](openmaker-logo.svg) | Full logo on a light background; transparent canvas |
| [openmaker-logo-dark.svg](openmaker-logo-dark.svg) | Full logo on a dark background; transparent canvas |
| [favicon.svg](../../favicon.svg) | Folder and cog on a rounded white tile |
| [favicon.png](../../favicon.png) | 256 x 256 PNG of the favicon |
| [favicon.ico](../../favicon.ico) | Favicon with 16, 32, 48, 64 and 256 pixel images |
| [openmaker-icon.png](../../crates/om-gui/assets/openmaker-icon.png) | The desktop client's app icon: the favicon's tile at 412 x 412 centered on a transparent 512 x 512 canvas, the macOS icon grid; `tools/package-macos.sh` makes the macOS app bundle's icon from it |

The README selects the appropriate logo for the reader's color scheme.
The favicon omits the lettering and keeps its white tile in either theme.
The files in the repository root are reusable assets; adding them to a
web application's HTML or routes is a separate integration.

To render a PNG from the vector favicon with VectorCraft:

```sh
vectorcraft-cli convert favicon.svg favicon.png --scale 0.5
```

The app icon was rendered with librsvg and ImageMagick:

```sh
rsvg-convert -w 412 -h 412 favicon.svg -o tile.png
magick -size 512x512 xc:none tile.png -gravity center -composite -strip \
  PNG32:crates/om-gui/assets/openmaker-icon.png
```

The artwork is covered by the repository's [MIT license](../../LICENSE).
