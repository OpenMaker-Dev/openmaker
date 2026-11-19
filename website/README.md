# openmaker.dev

The project's website: one static page with its CSS and script inline,
no build step and no third-party requests. Serve this folder as the site
root.

```sh
python -m http.server 4321 --directory website   # then open http://localhost:4321/
```

| File | Contents |
|---|---|
| `index.html` | The page |
| `assets/shots/*.webp` | Desktop client screenshots: `hero` (Browse, List view), `browse` (Table view), `find`, `layout` (Layout mode with the Inspector) |
| `assets/og.png` | 1200 x 630 link preview (`og:image`) |
| `favicon.svg`, `favicon.ico` | Copies of the repository root's favicons |

## Themes

The page follows the system's light or dark setting through
`prefers-color-scheme`; there is no switch on the page. Every colour is a
custom property: `:root` holds the dark values, and the
`@media (prefers-color-scheme:light)` block after it holds the light ones.
Add a new colour to both. In the light theme, text colours keep a contrast
of at least 4.5 to 1 against their backgrounds, the status badges' tinted
backgrounds included. The screenshots and `og.png` are the same in both
themes.

## Content that goes stale

The page restates facts kept elsewhere; update them together:

- **Status** section: each area's status and summary from
  [ROADMAP.md](../ROADMAP.md), with its date (2026-11-11), and the alpha
  gate's eight criteria.
- **Engines** section: the script-step count (about 130 of 210) from
  ROADMAP.md, and the calculation results from
  [docs/re/calc-values.md](../docs/re/calc-values.md) 2.7.
- **Clean room** counts: Markdown specifications in `docs/re/` other than
  its README (27), data files in `docs/re/catalogs/` other than its README
  (21) and crates in `crates/` (23).

## Screenshots

The screenshots are the desktop client's own rendering through
`egui_kittest`, as `crates/om-gui/tests/screenshots.rs` renders its PNGs,
at a 1060 x 760 point window and 2 pixels per point, on the mock provider
(`om_data::mock`). For the site, the mock's 42 records temporarily held
names of computer scientists instead of `First0`/`Last0`, and its
`is_sample` answered `false` so that the "Sample data" banner is not drawn;
neither change is committed. The PNGs were converted to WebP (quality 90).

Preview mode has no screenshot: the mock provider has no printable layout,
so its Preview shows "Preview unavailable".
