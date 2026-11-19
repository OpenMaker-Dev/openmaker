# openmaker.dev

The project's website: one static page with its CSS and script inline,
no build step and no third-party requests.

| File | Contents |
|---|---|
| `index.html` | The page |
| `assets/shots/*.webp` | Desktop client screenshots: `hero` (Browse, List view), `browse` (Table view), `find`, `layout` (Layout mode with the Inspector) |
| `assets/og.png` | 1200 x 630 link preview (`og:image`) |
| `favicon.svg`, `favicon.ico` | Copies of the repository root's favicons |
| `wrangler.jsonc`, `_headers`, `.assetsignore` | Deployment settings, not served ([Deployment](#deployment)) |

## Deployment

Cloudflare Workers Builds publishes this folder to https://openmaker.dev
as Workers static assets, with no Worker script. Each merge to `main` that
changes a file under `website/` deploys it; other merges start no build.
The connected repository and the build settings (root directory
`website`, deploy command `npx wrangler deploy`, build watch path
`website/*`) are kept in the Cloudflare dashboard. These files hold the
rest:

- `wrangler.jsonc`: the Worker's name, `openmaker-site`, which must match
  the Worker's name in the Cloudflare dashboard or the build fails; the
  folder to serve; the Custom Domain `openmaker.dev`; and no
  `*.workers.dev` address or Preview URLs. Each deploy sets the Worker's
  Custom Domains to the list in this file, so add a hostname here, not in
  the dashboard.
- `_headers`: headers for every response. The Content-Security-Policy
  allows the page's inline script and styles and images from this site,
  and blocks every other load, framing, `<base>` and form submission. An
  edit to `index.html` that loads something new (another origin, a
  `data:` image, a `fetch`) must widen the policy, or browsers block it.
  There is no `Strict-Transport-Security` header: `.dev` is on the
  browsers' HSTS preload list.
- `.assetsignore`: files not uploaded, in `.gitignore` syntax: this
  README, `wrangler.jsonc`, local tool output and local `.env` and
  `.dev.vars` files. Wrangler never uploads
  `_headers`, `_redirects` or `.assetsignore` themselves.

Every other file in this folder is public at https://openmaker.dev/. Keep
account ids, zone ids, tokens and other private details out of
`website/`; the configuration needs none of them.

To preview the site as Cloudflare serves it, with the headers and the
ignore list applied:

```sh
cd website && npx wrangler dev   # then open http://localhost:8787/
```

It needs no Cloudflare login, and the `.wrangler/` folder it writes is
ignored by git and by the upload. A plain file server, such as
`python -m http.server 4321 --directory website`, shows the page but
applies neither `_headers` nor `.assetsignore`.

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
