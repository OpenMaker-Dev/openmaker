# Page Setup and shared paper settings

File > Page Setup and the Preview toolbar's Page Setup button open the same
portable desktop modal used by Print Setup (script step 42). It offers US Letter,
Legal, Tabloid, A3/A4/A5, custom width/height in points (72 points = one inch), and
portrait/landscape. Custom dimensions are the unrotated sheet, 1–200 inches on each
axis. Landscape swaps the axes once. A layout prints within its stored fixed margins
when Layout Setup's "Use fixed page margins" is checked, and otherwise within
the printer's margins, 18 / 40 / 18 / 18 points (top, bottom, left, right), for
every paper ([portable PDF](portable-pdf.md#supported-layout-and-output-contract), #609).
Page Setup does not edit margins or layout metadata; a layout's fixed
page margins are Layout Setup's (Printing tab; [desktop
client](desktop-client.md), "Layout Setup"). A paper choice can leave a
layout wider than the printable width, which then prints what fits
([wide layouts](portable-pdf.md#wide-layouts)); a part taller than the printable
height refuses output.

**Scale** is 1% to 1000%, 100% by default. The layout prints at that fraction
of its size: the page, in layout points, is the sheet divided by the scale, so
at 50% a page holds twice as much of the layout each way. This is how
FileMaker stores a scaled setup: the sheet as it is, and the printable size
divided by the scale (script-steps.md 11.6). Preview draws the page at the
sheet's size with the layout scaled on it: up to 100% its text is laid out
at the size it prints; above 100% the layout is drawn at its own size and
magnified, as zooming in does. Save Records as PDF writes the sheet as each
page's media box and scales the page's content onto it; Print sends that
PDF on the sheet. Pictures, SVGs and button icons are sampled for the size
they print at. The printer's margins scale with the page, so they stay
18 / 40 / 18 / 18 points on paper. Inferred, unverified parity
([#871](https://github.com/OpenMaker-Dev/openmaker/issues/871)): the 1%–1000%
bounds are OpenMaker's (FileMaker Pro's are unmeasured; Claris documents scale
only as a macOS Page Setup option), and so are a layout's fixed page margins
scaling with the page and how Preview shows a scaled page; FileMaker's
Preview and PDF output at a scale were not compared.

Accepted paper settings belong to the open **file session**, apply to every
layout and window of that file, and are inherited by new windows. Other files,
including external-script files with colliding layout IDs, retain their own
settings. Preview, script page navigation and Save Records as PDF all obtain their
physical page size from `om_layout::paper::PaperSettings`. Changing paper drops
only the physical report plan, resets Preview to page one and changes its scroll
identity. Accepted record pages, fetch keys, pending ranges, errors, found sets,
Browse selection and record drafts retain their existing lifecycle.

A file that stores a page setup (`[2, 87]`, [file format](re/file-format.md)
3.4: the same tagged chunks as Print Setup's page format) opens with it: when its
`Comm` chunk reads (below), it is the file session's paper until one is accepted
in the session, in every window (also one a script opens the file in), Preview,
Print and Save Records as PDF, and it seeds the first dialog. A stored setup
larger than 1 MiB, by its declared or actual size, is ignored before it is
reassembled (`om_data::meta::page_format`), and so is one that does not read.
Without one,
the Letter/layout-orientation defaults remain and the first dialog is seeded from
the current layout. None of the starter solutions stores one.

Once accepted, the preference overrides layout orientation for every layout.

**Saving it in the file.** A Page Setup accepted in its dialog without
Restore is saved in a file on this computer at the OK, as FileMaker Pro saves
it ([file format](re/file-format.md) 3.8,
[#876](https://github.com/OpenMaker-Dev/openmaker/issues/876)). That covers
File > Page Setup, the Preview toolbar's button, and a script's Print Setup
with the dialog and Restore off.
- **What is written:** `[2, 87]` holds one `Comm` chunk
  (`PaperSettings::common_format`), stored chunked as Pro stores it.
  `DataProvider::store_page_format` locks the key, checks that the file
  still stores what the decision was made on, uploads it and reads it
  back. FileMaker Pro reads such a page setup whole, in its Page Setup sheet
  and its output, and adds its own platform records at its next OK.
- **Platform records:** the file's `MMod` copies, and its `M_PM` when it
  is an Any Printer record for one of the printer's papers, go with a
  change: Pro makes those again from `Comm` (file-format.md 3.8). Kept
  beside a new `Comm`, Pro's sheet would show their old settings while its
  output used the new ones, and its next OK would return to them.

Nothing is written when:
- the file's setup already reads as the accepted paper, so an OK without
  changes keeps Pro's records;
- the file stores a setup OpenMaker cannot read, or its schema was not read,
  so what it stores is not known: it is kept as it is;
- the file's setup holds a record Pro may not make again: a custom paper
  or a named printer in `M_PM`, or a record other than `Comm`, `M_PM` and
  `MMod`, a Windows one say. It is also kept when it is not stored as Pro
  stores it, or changed since it was read;
- the file is hosted. FileMaker Pro sends nothing for a hosted file ([write path](re/write-path.md)
  5.9): the setup lasts while the file is open in that client, and so it
  does in OpenMaker;
- the step restores a stored setup. FileMaker Pro's restore without the dialog
  left the file's setup as it was; with the dialog, Pro's behavior is
  unmeasured;
- the account does not have [Full Access], or the file is read-only. The
  in-process host stores a direct upload only for [Full Access]; what
  FileMaker Pro saves for other accounts is unmeasured.

In each of these cases the setup is the session's. A save that is refused
or fails is logged.
The `Comm` carries OpenMaker's sheet sizes and printer margins, 18 / 40 /
18 / 18, which Pro reads as they are. Pro resaves OpenMaker's A4, A3 and A5,
which are not its own sizes, as custom papers of the same size at its next
OK ([#960](https://github.com/OpenMaker-Dev/openmaker/issues/960)). How
FileMaker Pro for Windows reads a page setup without its own record is
unmeasured ([#955](https://github.com/OpenMaker-Dev/openmaker/issues/955)).

## Script step 42

- Restore Off + With dialog On opens current file settings (or the initial default).
- Restore On stages the supported common page format from raw key `87`; With
  dialog On lets the user change it before acceptance. Cancel returns error 1 and
  retains the prior preference. With dialog Off applies it without a prompt.
- Restore Off + With dialog Off succeeds without changing settings. Stale stored
  page formats are ignored. Malformed/unsupported restored formats return error 3
  with a diagnostic through the normal error-capture/alert path.
- Validation and cancellation never partially replace settings and never commit
  or revert records. The operation does not dispatch Print (43), select a printer,
  open a hardware dialog or submit a job.

The decoder accepts exactly one 29-byte `Comm` chunk: orientation 0/2, scale 1% to
1000%, finite paper rectangle and positive printable dimensions within the sheet,
turned upright for landscape and multiplied back by the scale. The
step bounds the value to 1 MiB before reassembly, as opening a file bounds its
stored setup, validates every chunk boundary and
rejects missing/duplicate common chunks, invalid sizes/numbers, and scales
outside 1% to 1000%. Paper size is `(right-left, bottom-top)`; printable-origin offsets are
validated but do not replace the printer's margins. The rectangle is interpreted
as the unrotated sheet and orientation is applied once; the printable size is
the page's own, so landscape stores it turned, and divided by the scale
(script-steps.md 11.6). The
common chunk a FileMaker Pro 26 for macOS file stored at `[2, 87]` reads this
way as US Letter portrait (the sheet -18, -18 to 594, 774 around a 576 x 734
printable area), and one a Print Setup step stored as US Letter landscape (the
same sheet around 734 x 576), so the big-endian float order holds for native
bytes; other papers are still synthetic. The landscape chunk at 60% and 40%
is built from it the way [#817](https://github.com/OpenMaker-Dev/openmaker/issues/817)
shows FileMaker stores those: the printable
size 1223.33 x 960 and 1835 x 1440.

The tagged stream's big-endian u32 lengths follow the existing
`om-scripts/examples/stored_specs.rs` decoder/re-encoder and
`docs/re/script-steps.md` 11.0/11.6. Common f32 values use big-endian order, inferred
from the Swap-stream convention and the existing BE paragraph-style f32 decoder
in `om-layout/src/rich_text.rs`. Native chunks confirm it: the US Letter
portrait and landscape chunks above. No other native paper was compared.
Original key `87` bytes, including opaque `M_PM`, `M_PD`, `MMod`, printer names and
unknown chunks, remain intact in the script codec and session source record. UI
edits change the portable paper preference, not those source bytes. Undecoded
platform records have no portable effect; preserving them does not apply printer,
driver, tray, duplex, resolution or other native options. Key `88` remains untouched,
as Page Setup leaves it in FileMaker Pro.

## Acceptance and limits

Four focused new checks cover literal common-format decoding/preservation/refusal,
script restore + external-file ownership + vector PDF output, cancellation and
invalid-restore preservation of dirty records, and actual menu/toolbar dialog
accept/reopen with Preview repagination. The existing Preview isolation test also
checks cached-plan replacement, same-file/new-window inheritance and peer-file
isolation while retaining its stale-range/reload-error checks. These use the
existing synthetic report/provider and literal synthetic page-format bytes, and
the decoder check also reads the two native US Letter `Comm` chunks, portrait and
landscape, and the landscape one at 60% and 40%. The synthetic landscape formats
are turned the way FileMaker stores them, with the printable size swapped. A
scale check saves the nine-record fixture as PDF on A4 at 100% (three pages)
and, after Print Setup restores 50%, on one A4 page with its content scaled.

A4 landscape produces five physical pages for the nine-record fixture; both
Preview and PDF use that plan, including report page-number calculations. Existing
ordinary report limitations still apply ([Preview](preview.md),
[portable PDF](portable-pdf.md)); sorted sub-summary groups share the same plan
([report sections](report-sections.md)), as do objects that slide up
([report sliding](report-sliding.md)); other advanced parts remain separate
work (C41 in [client-progress.md](client-progress.md)). Desktop Print (43) now uses the same report foundation; see [printing](printing.md) for its dispatcher and limits. Platform print-record
interpretation is unimplemented; this is not a 1:1 native printing claim. Saving
in the file is checked against OpenMaker's local host: the setup is written,
stored chunked and read back after a reopen. The menu test checks which OKs
save one.

Primary semantics: [Claris printer and paper options](https://help.claris.com/en/pro-help/content/printer-paper-options.html)
(per-file paper/orientation, all layouts, menu/toolbar, macOS scale) and
[Claris Print Setup](https://help.claris.com/en/pro-help/content/print-setup.html)
(With dialog, Restore and printer behavior). Applying the shared paper model to
desktop portable PDF is OpenMaker's implementation contract; the Claris page
explicitly documents the PDF effect for Server/Cloud/WebDirect.
