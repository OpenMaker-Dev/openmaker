# Page Setup and shared paper settings

File > Page Setup and the Preview toolbar's Page Setup button open the same
portable desktop modal used by Print Setup (script step 42). It offers US Letter,
Legal, Tabloid, A3/A4/A5, custom width/height in points (72 points = one inch), and
portrait/landscape. Custom dimensions are the unrotated sheet, 1–200 inches on each
axis. Landscape swaps the axes once. Scale is fixed at 100%; there is no control
suggesting other scales work. A layout prints within its stored fixed margins
when Layout Setup's "Use fixed page margins" is checked, and otherwise within
the printer's margins, 18 / 40 / 18 / 18 points (top, bottom, left, right), for
every paper ([portable PDF](portable-pdf.md#supported-layout-and-output-contract), #609).
Page Setup does not edit margins or layout metadata. A paper choice can leave a
layout wider than the printable width, which then prints what fits
([wide layouts](portable-pdf.md#wide-layouts)); a part taller than the printable
height refuses output.

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
in the session, in every window, Preview, Print and Save Records as PDF, and it
seeds the first dialog. A stored setup that does not read is ignored. Without one,
the Letter/layout-orientation defaults remain and the first dialog is seeded from
the current layout. None of the starter solutions stores one.

Nothing is written back: there is **no local disk persistence** and no write to
the hosted file, so a Page Setup accepted in OpenMaker is lost when the file
closes, and the file keeps the setup it had. Once accepted, the preference
overrides layout orientation for every layout. This differs from native
FileMaker, which saves the choice in the file.

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

The decoder accepts exactly one 29-byte `Comm` chunk: orientation 0/2, scale 1.0,
finite paper rectangle and positive printable dimensions within the sheet. It
bounds the value to 1 MiB before reassembly, validates every chunk boundary and
rejects missing/duplicate common chunks, invalid sizes/numbers, and scale other
than 100%. Paper size is `(right-left, bottom-top)`; printable-origin offsets are
validated but do not replace the printer's margins. The rectangle is interpreted
as the unrotated sheet and orientation is applied once. The common chunk a
FileMaker Pro 26 for macOS file stored at `[2, 87]` reads this way as US Letter
portrait (the sheet -18, -18 to 594, 774 around a 576 x 734 printable area), so
the big-endian float order holds for native bytes; landscape and other papers
are still synthetic.

The tagged stream's big-endian u32 lengths follow the existing
`om-scripts/examples/stored_specs.rs` decoder/re-encoder and
`docs/re/script-steps.md` 11.0/11.6. Common f32 values use big-endian order, inferred
from the Swap-stream convention and the existing BE paragraph-style f32 decoder
in `om-layout/src/rich_text.rs`; no native orientation/float comparison was run.
Original key `87` bytes, including opaque `M_PM`, `M_PD`, `MMod`, printer names and
unknown chunks, remain intact in the script codec and session source record. UI
edits change the portable paper preference, not those source bytes. Undecoded
platform records have no portable effect; preserving them does not apply printer,
driver, tray, duplex, resolution or other native options. Key `88` remains untouched.

## Acceptance and limits

Four focused new checks cover literal common-format decoding/preservation/refusal,
script restore + external-file ownership + vector PDF output, cancellation and
invalid-restore preservation of dirty records, and actual menu/toolbar dialog
accept/reopen with Preview repagination. The existing Preview isolation test also
checks cached-plan replacement, same-file/new-window inheritance and peer-file
isolation while retaining its stale-range/reload-error checks. This uses only the
existing synthetic report/provider and literal synthetic page-format bytes.

A4 landscape produces five physical pages for the nine-record fixture; both
Preview and PDF use that plan, including report page-number calculations. Existing
ordinary report limitations still apply ([Preview](preview.md),
[portable PDF](portable-pdf.md)); sorted sub-summary groups share the same plan
([report sections](report-sections.md)), as do objects that slide up
([report sliding](report-sliding.md)); other advanced parts remain separate
work (C41 in [client-progress.md](client-progress.md)). Desktop Print (43) now uses the same report foundation; see [printing](printing.md) for its dispatcher and limits. Native FileMaker
acceptance, platform print-record interpretation and native per-file persistence
are unmeasured/unimplemented; this is not a 1:1 native printing claim.

Primary semantics: [Claris printer and paper options](https://help.claris.com/en/pro-help/content/printer-paper-options.html)
(per-file paper/orientation, all layouts, menu/toolbar, macOS scale) and
[Claris Print Setup](https://help.claris.com/en/pro-help/content/print-setup.html)
(With dialog, Restore and printer behavior). Applying the shared paper model to
desktop portable PDF is OpenMaker's implementation contract; the Claris page
explicitly documents the PDF effect for Server/Cloud/WebDirect.
