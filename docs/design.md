# OpenMaker's design

How OpenMaker's own interface looks: its colors, type, shapes and the rules
chrome code follows. It governs everything OpenMaker draws itself in the
desktop client: windows, bars, menus, dialogs, Home and the developer tools.
It does not govern the content area, which renders the file's own layouts,
themes and fonts (`docs/re/layout-model.md`); a solution's design belongs to
its developer. Nor does it govern OpenMaker Server's Admin Console, which
reproduces FileMaker Server's console ([admin-console.md](admin-console.md)),
or the website, which carries OpenMaker's orange brand color
(`website/index.html`); the brand color is not a chrome color.

The tokens live in `crates/om-gui/src/theme.rs`. This document is the
reference for them; the code is the source of truth for values.

## Why Apple's guidelines

FileMaker Pro on macOS is an AppKit application, so its chrome takes
Apple's system appearance: its toolbar, dialogs, menus and lists are drawn
in macOS's semantic colors and metrics (`docs/re/client-ui.md`). Following
Apple's [Human Interface Guidelines](https://developer.apple.com/design/human-interface-guidelines/)
(HIG) therefore serves both of OpenMaker's aims: a familiar interface for
people moving from FileMaker, and a consistent one where FileMaker gives no
reference (OpenMaker-only dialogs).

Two sources set the values, in this order:

1. **Measured.** Where a region of FileMaker Pro 26 on macOS was measured
   from the captures (toolbar, layout bar, pasteboard, Hosts sidebar, menu
   rows, Home), the measured value is the token, as `AGENTS.md` rule 9 asks.
2. **Apple.** Everything else uses Apple's published values: the 2025
   system colors from the HIG's [Color](https://developer.apple.com/design/human-interface-guidelines/color)
   page (updated 2025-06-09) and the macOS text styles from
   [Typography](https://developer.apple.com/design/human-interface-guidelines/typography).

The HIG asks apps not to hard-code system color values and to read them
through AppKit instead. OpenMaker draws with egui on every platform and
cannot read `NSColor`, so the tokens carry the published values as
constants. A new macOS release can change Apple's values; the tokens then
change here, deliberately.

## Principles

- **Content first.** Chrome is neutral gray and recedes. Color appears only
  where it means something: selection, the default action, focus, status.
- **One accent.** A single blue marks every selected, default, highlighted
  and focused state. No screen brings its own brand color.
- **One meaning per color.** Red is an error or a destructive action, and
  nothing else. Blue is selection or the default action, never decoration.
  The HIG asks for this ("Avoid using the same color to mean different
  things").
- **Tokens, not literals.** Chrome code names a token by its role
  (`theme::CAPTION`, `theme::BORDER`) and never writes a color literal. Two
  grays that differ by a few levels for the same role are one token.
- **Legible.** Text keeps 4.5:1 contrast against its background up to
  17 pt, and 3:1 at 18 pt or bold; non-text indicators keep 3:1. These are
  the WCAG AA values the HIG's [Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility)
  page uses.
- **Not color alone.** State is also carried by shape, text or a glyph: a
  filled versus an outlined star, a check mark, a message's words.
- **Two appearances.** Every token has a light and a dark value, even while
  only light ships (Limits). The HIG asks for both "even if your app ships
  in a single appearance mode".

## Color

Values are sRGB. "Dark" is the planned dark appearance (Limits); its values
are OpenMaker's choice, derived from macOS's dark semantic colors, and are
unmeasured.

### Accent

| Token | Use | Light | Dark |
|---|---|---|---|
| `ACCENT` | The one blue: selected controls and rows, the default button, the highlighted menu row, the text cursor, links | `#1E6EF4` | `#1E6EF4` fills, `#5CB8FF` text |
| `SELECTED`, `HIGHLIGHT`, `DEFAULT_BUTTON` | Role names for `ACCENT`: a selected control or row, the hovered or open menu row, the default button | = `ACCENT` | = `ACCENT` |
| `ON_ACCENT` | Text and glyphs on an `ACCENT` or `RED` fill | `#FFFFFF` | `#FFFFFF` |
| `FOCUS` | The keyboard focus ring: the accent at three quarters, the palest that keeps 3:1 against a white field | `#5692F7` | `#5CB8FF` |
| `CURRENT_ROW` | The current row of a list: the accent at a tenth | `#E8F0FE` | `#1E2A3E` |
| `SELECTED_TEXT_BG` | Selected text | `#B4D5FE` | `#3F638B` |
| `RING` | The found-set ring in the status toolbar (measured) | `#7AAFE6` | `#3D6C9E` |

`ACCENT` is Apple's Blue in its increased-contrast variant (`R30 G110 B244`).
Apple's default Blue (`#0088FF`) leaves white labels at 3.5:1, below the
4.5:1 that a 13 pt button label needs; the increased-contrast variant gives
4.57:1. OpenMaker uses that one blue everywhere rather than two blues for
fills and lines.

### Status

Apple's system colors in their increased-contrast variants, which keep
4.5:1 on white. Tints are 10 to 16 percent of the color over white.

| Token | Use | Light | Dark |
|---|---|---|---|
| `RED` | Errors, failures, destructive actions | `#E9152D` | `#FF6165` |
| `RED_TINT` | Background of an error banner or marked row | `#FDE8EA` | `#4A1E22` |
| `RED_INK` | Text on `RED_TINT`: the red darkened to keep 4.5:1 | `#B80E28` | `#FF6165` |
| `YELLOW` | Warning text | `#A16A00` | `#FEDF43` |
| `YELLOW_TINT` | Background of a warning banner | `#FFF7D6` | `#3D3416` |
| `YELLOW_INK` | Text on `YELLOW_TINT`: the yellow darkened to keep 4.5:1 | `#865700` | `#FEDF43` |
| `CAUTION` | The caution glyph's fill, with `TEXT` ink (Apple's default Yellow) | `#FFCC00` | `#FFD600` |

When a screen needs a status hue that has no token yet, take it from the
same HIG table and add a token: Green `#008932` (dark `#4AD968`), Orange
`#C55300` (dark `#FFA056`).

The window controls drawn on in-window dialogs use macOS's own colors in
both appearances: `WINDOW_CLOSE` `#FF5F57`, `WINDOW_MINIMIZE` `#FEBC2E`,
`WINDOW_ZOOM` `#28C840`.

### Surfaces

| Token | Use | Light | Dark |
|---|---|---|---|
| `CONTROL_BG`, `CONTENT_BG`, `INSPECTOR_BG` | Lists, tables, text fields; the content area; the Inspector | `#FFFFFF` | `#1E1E1E` |
| `ALTERNATE_ROW` | Every other row of a striped list (Apple's alternating content color) | `#F4F5F5` | `#292929` |
| `DIALOG_BG` | Window background: dialogs, panes, sheets | `#F0F0F0` | `#323232` |
| `TOOLBAR_BG` | The status toolbar and title (measured) | `#F4F4F4` | `#383838` |
| `MENU_FILL` | Menus and popovers | `#F5F5F5` | `#2C2C2C` |
| `SEARCH_FILL` | Search fields in bars | `#EEEEEE` | `#3A3A3A` |
| `BAR_BG` | Button strips under lists, bars inside dialogs | `#ECECEC` | `#2B2B2B` |
| `SIDEBAR_BG` | Sidebars: Hosts (measured) | `#E3E3E4` | `#2A2A2A` |
| `CAPSULE_FILL` | Capsule buttons: Home's Create and Open (measured) | `#ECECEC` | `#3A3A3A` |
| `NAV_SELECTION` | The selected page in Home's sidebar (measured in an inactive window) | `#E9E9EB` | `#353537` |
| `LAYOUT_BAR_BG` | The layout bar (measured) | `#DEDEDF` | `#262626` |
| `HOVER_FILL` | A hovered or pressed borderless control; a selection in a list without keyboard focus | `#DCDCDC` | `#464646` |
| `PASTEBOARD` | Layout mode's pasteboard (measured) | `#E5E5E5` | `#262626` |
| `PRINT_WORKSPACE` | Behind pages: Preview, a container's PDF view (measured) | `#9C9C9C` | `#282828` |
| `SCRIM` | The dimming behind a modal dialog | black 9% | black 30% |

### Text and glyphs

| Token | Use | Light | Dark |
|---|---|---|---|
| `TEXT` | Primary text (Apple's label color, black at 85%) | `#262626` | `#D9D9D9` |
| `CAPTION` | Secondary text: captions, column headers, locations | `#5E5E5E` | `#A3A3A3` |
| `WEAK_TEXT` | Hints and placeholders only, never essential text | `#8A8A8A` | `#7F7F7F` |
| `DISABLED` | Unavailable text and glyphs | `#C4C4C4` | `#5A5A5A` |
| `ICON` | Monochrome glyphs in bars and lists (measured) | `#7D7D7D` | `#9A9A9A` |
| `VIEW_GLYPH` | Unselected view-switcher glyphs (measured) | `#A5A5A6` | `#6E6E6E` |

### Lines

Lightest to darkest.

| Token | Use | Light | Dark |
|---|---|---|---|
| `SEPARATOR` | Rules between rows and sections inside a pane (Apple's grid color) | `#E6E6E6` | `#383838` |
| `SEARCH_BORDER` | Search fields in bars | `#E0E0E0` | `#3C3C3C` |
| `HAIRLINE` | Pane edges, the frames of grouped content, menu separators | `#D6D6D6` | `#3F3F3F` |
| `BORDER` | Text fields, list boxes, the frames of in-window dialogs | `#C5C5C5` | `#4D4D4D` |
| `BUTTON_BORDER` | Push buttons and other bezels | `#B9B9BA` | `#555555` |
| `DIVIDER` | Dividers between toolbar groups and bar sections | `#B4B4B5` | `#5A5A5A` |
| `POPUP_BORDER` | Pop-up menus, a pressed button's outline | `#9B9B9C` | `#6E6E6E` |

The layout bar's rules and the menu bar's rule are measured region colors
(`LAYOUT_BAR_TOP_RULE`, `LAYOUT_BAR_BOTTOM_RULE`, `MENU_BAR_RULE`).

### Contrast

Measured with the WCAG formula over the light surfaces:

| Foreground | Lowest ratio | On |
|---|---|---|
| `TEXT` | 11.0:1 | `HOVER_FILL` |
| `CAPTION` | 4.7:1 | `HOVER_FILL` |
| `ON_ACCENT` | 4.6:1 | `ACCENT` |
| `ACCENT`, `RED`, `YELLOW` as text | 4.6:1 | `CONTROL_BG` |
| `ACCENT`, `RED`, `YELLOW` as text | 4.0:1 | `DIALOG_BG` |
| `RED_INK`, `YELLOW_INK` | 5.7:1 | `RED_TINT`, `YELLOW_TINT` |
| `ICON` | 3.0:1 | `HOVER_FILL` |
| `WEAK_TEXT` | 2.5:1 | `HOVER_FILL` |

So: put colored text (`ACCENT`, `RED`, `YELLOW`) on `CONTROL_BG`, or make it
13 pt semibold or 18 pt on darker surfaces, where 3:1 is enough. On a tint,
text takes the tint's ink (`RED_INK`, `YELLOW_INK`). Keep
`WEAK_TEXT` to hints that repeat what the control already says.

### Exceptions

These keep their own colors, because they are FileMaker's or the file's:

- The calculation editor's syntax colors (functions `#00A8A9`, numbers
  `#AB2079`, text `#C2410C`, comments `#5E864D`), measured from capture 42
  (`script_workspace/calculation/syntax.rs`).
- The relationship graph's own drawing: black lines, white table bodies and
  the table occurrence colors stored in the file.
- Preview's paper and everything the layout draws on it.
- Image tints (`Color32::WHITE` passed to `painter.image` leaves an image
  unchanged) and test data.

A module that needs a chrome color with no fitting role (a card window's
shadow, a printable-area guide) keeps it as a named `const` with a comment
giving its role. Find them with
`rg -n "const [A-Z_]+: Color32" crates/om-gui/src --glob '!theme.rs'`; a
second use of the same role makes it a token.

## Type

OpenMaker bundles Inter (regular and semibold) as its interface font, in
place of Apple's SF Pro, which may not be shipped outside Apple platforms.
Inter has similar metrics and renders the same on every platform. Emphasis
uses the semibold face (`theme::semibold()`); there is no bold UI face.

Sizes follow macOS's built-in text styles:

| Token | macOS style | Size / line | Use |
|---|---|---|---|
| `SMALL` | Subheadline | 11 / 14 | Toolbar captions, layout-bar labels, column headers |
| `BODY` | Body | 13 / 16 | Menus, dialogs, lists, buttons |
| `TITLE3` | Title 3 | 15 / 20 | Section headings; egui's `Heading` |
| `TITLE2` | Title 2 | 17 / 22 | The app's name in Home's header |
| `PAGE_TITLE` | (measured) | 19 | A page's heading in a window: Home's, measured from FileMaker |

Code uses egui's monospace at 12 pt. Add a token from the same table when a
screen needs another size (Large Title 26, Title 1 22, Callout 12,
Footnote 10). Window titles are 13 pt semibold, as macOS draws them.

A command that needs more input before it acts ends in an ellipsis
character (`Open File…`, not `Open File...`), as the HIG and FileMaker do.

## Shape and spacing

| Token | Value | Use |
|---|---|---|
| `CONTROL_RADIUS` | 5 pt | Buttons, fields, list and sidebar rows |
| `MENU_RADIUS` | 7 pt | Menus, popovers, grouped lists |
| `WINDOW_RADIUS` | 10 pt | Windows, sheets, dialogs |
| `MENU_ROW_HEIGHT` | 22 pt | Menu rows |
| egui `interact_size` | 22 pt | Regular controls (macOS's regular size) |

Region heights are measured: menu bar 24, status toolbar 54, layout bar 25,
formatting bar 27; the Inspector is 252 pt wide. Home's geometry is
FileMaker's, measured (`home.rs`, `docs/re/client-ui.md`): a 51 pt header,
a 198 pt sidebar and 50 pt rows.

Spacing is OpenMaker's choice, in multiples of 4 pt where nothing measured
says otherwise: 20 pt between a window's edge and its content, 8 pt between
related controls, 12 to 20 pt between groups. egui's default item spacing is
6 by 4 pt (`theme::style`).

Windows cast a soft shadow (offset 10, blur 30, black at 24%); menus and
popovers a smaller one (offset 4, blur 14, black at 18%).

## Components

- **Push button.** `CONTROL_BG` face, `BUTTON_BORDER` outline, `TEXT`
  label, `CONTROL_RADIUS`. Hover lightens nothing but the outline; pressed
  fills `HOVER_FILL`.
- **Default button.** `DEFAULT_BUTTON` fill with an `ON_ACCENT` label and no
  outline, flat as macOS draws it since macOS 11. One per dialog, the
  Return-key action, at the trailing end.
- **Text field.** `CONTROL_BG`, `BORDER`, `FOCUS` ring when focused,
  `SELECTED_TEXT_BG` behind selected text, `WEAK_TEXT` placeholder.
- **List and table.** `CONTROL_BG` with `ALTERNATE_ROW` stripes inside a
  `HAIRLINE` frame; `CAPTION` column headers in `SMALL` semibold; a selected
  row is `SELECTED` with `ON_ACCENT` text, or `HOVER_FILL` when the list does
  not have keyboard focus.
- **Sidebar.** `SIDEBAR_BG` with `CONTROL_RADIUS` rows; the selected row is
  `SELECTED` with `ON_ACCENT` text, a hovered row `HOVER_FILL`. Hosts uses
  it.
- **Home.** FileMaker's own launcher look, measured: white surfaces
  (`CONTENT_BG`) with `SEPARATOR` rules; `CAPSULE_FILL` capsule buttons
  with `TEXT` labels, `HOVER_FILL` when pressed; sidebar pages in `CAPTION`
  with `ICON` glyphs, the selected one on `NAV_SELECTION`; tabs in
  `CAPTION`, the selected one underlined 2 pt in `ACCENT`; and a frameless
  table of 50 pt rows, `TEXT` names and locations, a selected row
  `SELECTED` with `ON_ACCENT` text.
- **Menu.** `MENU_FILL`, `HAIRLINE` outline and separators, `MENU_RADIUS`,
  22 pt rows; the hovered or open row is `HIGHLIGHT` with `ON_ACCENT` text.
- **Messages.** Error text is `RED`. An error banner is `RED_TINT` with
  `RED_INK` text; a warning banner is `YELLOW_TINT` with `YELLOW_INK` text. The caution
  glyph is `CAUTION` with `TEXT` ink, the stop glyph `RED` with `ON_ACCENT`
  ink.
- **Glyphs.** Monochrome, `ICON` at rest, `SELECTED` when on, `DISABLED`
  when unavailable. OpenMaker draws its own glyphs (`icons.rs`); SF Symbols'
  license limits them to Apple platforms.

## Changing the design

1. Name the role first. Use the existing token for it; if none fits, add one
   to `theme.rs` with a doc comment, and a row here with its light and dark
   values and where the value comes from.
2. Check contrast for the text or glyph that will sit on it.
3. Look at the result (`cargo test -p om-gui --test screenshots`, see
   [development.md](development.md#verifying-ui-changes)) and keep
   before and after images for the pull request.

## Limits

- **Light only.** OpenMaker pins the light appearance (`theme::install`);
  the dark values above are planned and unverified
  ([#648](https://github.com/OpenMaker-Dev/openmaker/issues/648)).
  Whether FileMaker Pro 26's chrome follows macOS's dark appearance is
  unmeasured.
- **Selections in egui lists and text boxes.** egui draws a selected
  `selectable_label`'s text, selected text and a focused text box's outline
  in one color, `FOCUS`, so selected text is pale blue on `SELECTED_TEXT_BG`
  rather than `TEXT` on it, and a selected list row is not `SELECTED` with
  `ON_ACCENT` text
  ([#267](https://github.com/OpenMaker-Dev/openmaker/issues/267)).
- **Fixed accent.** The accent does not follow the accent color or the
  Increase Contrast setting chosen in macOS's System Settings.
- **Not Liquid Glass.** FileMaker Pro 26.1.1 supports macOS 27's Liquid
  Glass look (`docs/re/client-ui.md`); OpenMaker draws opaque surfaces.
- **Blues differ from the captures.** The captures show
  `#2F54E5` for the selected view glyph, `#3E62EC` for the default button,
  and a `#3D4FD1` to `#1429C8` gradient on Specify Calculation's default
  button (capture 42), and FileMaker 26.1.1's Home underlines its selected
  tab in `#2D55E7` on a Mac set to the default accent color, so the accent
  setting is not their cause. OpenMaker uses `ACCENT` for all of them.
  Whether they come from the display profile or FileMaker's own drawing is
  unmeasured.
