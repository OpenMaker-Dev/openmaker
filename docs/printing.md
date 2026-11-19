# Desktop Print

File > Print (Cmd/Ctrl+P), Preview's Print toolbar button and script Print (43)
share one owned print flow. On macOS and Linux it submits an ordinary vector PDF
to CUPS using `/usr/bin/lp`; Windows returns command unavailable (3). CUPS must be
installed and have an available PDF-capable print queue/filter chain. No native
hardware dialog, screenshot or CSV conversion is used.

The portable dialog offers the CUPS system/user default destination, destinations
reported by `lpstat -e`, 1-999 copies, records being browsed or the current record,
All pages or a contiguous inclusive From/To range, and Cancel. Choices apply to
this job only: selecting a printer does not change
CUPS defaults or establish an OpenMaker printer preference for subsequent jobs.
The default choice lets CUPS resolve its usual `LPDEST`/`PRINTER`/user/system
default. An explicit selected destination is passed literally with `-d`; if it
is unavailable the request fails, without falling back to another printer.

Paper/orientation, fixed layout margins, 100% scale, pagination, ordinary report
refusals and nested calculation page context use the existing shared
[Page Setup](page-setup.md), [Preview](preview.md) and [vector PDF](portable-pdf.md)
foundation. Print reuses PDF's report builder; found-set order and the current
Browse record are retained. [Title and grand-summary sections](report-sections.md)
use that same plan, including grand-only pages. Print submits the complete PDF
with an explicit physical-page selection, one page per sheet, explicit
media/orientation and no scaling. It requests strict IPP attribute
fidelity so unsupported job attributes can fail rather than be substituted. Standard paper sizes use CUPS media
names; other dimensions use CUPS's point-based `Custom.WIDTHxLENGTH` notation and
require a queue that supports that custom paper. Queue defaults govern finishing
such as duplex, trays and collation; there are no controls suggesting those
options are decoded from saved native settings.

## Physical page selection

All pages is the default for each new job. Page range accepts positive whole
From/To numbers with From <= To (up to CUPS's positive signed integer limit).
Malformed, zero or reversed bounds show an inline error and cannot submit,
including through Enter. All pages ignores inactive range inputs. Cancel and
Escape remain available with invalid inputs.

The range refers to physical pages of the complete current-record or found-set
report, after pagination and calculation. The worker checks the upper bound
against the shared report plan before creating a temporary PDF or submitting.
Out-of-document ranges return error 3 with the report's actual page count; they
are never silently clipped. This check follows the existing pending-edit commit
and trigger-veto flow, so a successful record commit is retained if the range
then fails. The generated PDF retains all records, original Get(PageNumber)
values, title parts and once-only full-found-set grand summaries. Records are
never sliced to implement a page range.

This dialog selection does not decode ranges from saved native Restore records.
Discontiguous selections, odd/even pages and numbering offsets remain unsupported.

## Scripts and ownership

With dialog On opens the portable dialog; Off submits directly with the current
file's paper settings, default printer, one copy and the step's record choice.
Restore Off ignores stale keys 87/88 and the stored record choice: Print takes
the records being browsed, as FileMaker Pro 26 opens its Print dialog on them
for such a step even when it stores Current record (#843; that Pro then prints
them is inferred from the dialog). With Restore On, value 1 (and the
unconfigured menu/default value 0) means found set; value 2 means current
record. Values 3–6 (blank forms) and other record variants return 3 before
committing or submitting.

Restore On requires key 88. This slice accepts exactly one 11-byte `Comm`
chunk: numbering offset 0, from 0/1, to 65535/9999, all-pages byte 1, copies 1–999
and final observed word 1. Restored ranges, numbering offsets and other final
word values refuse. Scalars/lengths follow the documented big-endian Swap-stream
convention; native common-option acceptance is unmeasured. If key 87 coexists,
its common page format is decoded by Page Setup's existing decoder and used for
this job only. It never changes the file's session paper preference.

Restored Print keys may contain only the supported `Comm` chunk. Saved printer
names (`PrNm`) have an unverified header; platform records (`M_PM`, `M_PD`,
`MMod`) can encode options that this backend cannot honor. All such chunks,
unknown tags, duplicates, malformed/truncated records and records over 1 MiB
refuse explicitly, so a Print step whose options FileMaker Pro stored (with
`PrNm`, `M_PM`, `M_PD` and `MMod`) does not run with Restore On. The original
script bytes are preserved by the existing codec; Print does not rewrite them.
The Script Workspace writes one byte of them: the records choice in `M_PM`'s
`printStyleSetting` (script-steps.md, Implementation notes (Script
Workspace)). With dialog On does not authorize silently
discarding undecoded Restore options. Print Setup (42) continues to control the
shared portable paper preference and has no printer selection; its opaque source
provenance is not interpreted as an implicit request to Print.

Cancel returns 1 before any record commit, file creation or job submission.
After acceptance, owning-window object-exit/record-commit triggers can veto;
their normal error 20 prevents submission. An external trigger resumes the
held request in its owning file/window. A change of layout, record or mode
before the held action resumes refuses rather than printing a different context.
Record validation/provider failures retain the existing dirty-record rules.
An accepted commit remains committed if later report rendering or spooling fails;
printing never reverts a successful data commit. Unsupported reports fail before
creating a temporary file or submitting a job. Browse and Preview are supported;
Find/Layout printing refuses.

## Submission and lifetime

The worker writes the fully validated vector report to a private temporary PDF,
flushes it, keeps it alive throughout synchronous `lp` submission, and removes it
on success, failure or cancellation before dispatch. Arguments use `Command`,
with separate values and `--` before the path; printer names, titles and paths
never pass through a shell. Nonzero command exit or spawn failure returns Print
error 600 with a diagnostic through the normal capture/alert path. Temporary
file failures return 801, unsupported requests 3, and no records 203.

All-pages jobs explicitly send `page-ranges=1-2147483647`, overriding a saved
destination range without changing the user's defaults. CUPS encodes this as the
full positive page range. Selected ranges send `page-ranges=FROM-TO`, including
`N-N` for a single page. Both default and explicit destinations receive exactly
one range override; finishing defaults remain in effect.

Success means CUPS accepted submission, not that hardware printed the pages.
Cancel in this dialog stops before submission; manage already queued jobs in the
OS print queue. This slice does not track hardware completion or cancel a job
already accepted by the spooler. Native dialogs, driver output, printable-area
acceptance and physical/native FileMaker parity remain unmeasured. No real
printer job or hardware dialog is used in automated validation: the dispatcher
is injected in the Print regressions, and unit-test builds also default to a
recording fake.

Focused checks cover common Restore/literal argument encoding for All and
selected pages with default/explicit destinations, script-to-vector report
identity and owned file/window paper, cancel/refusal/error cleanup, range bounds
and job-local defaults, external commit-veto/resume, and actual dialog input
through menu/shortcut/Preview-toolbar dispatch. The range UI check uses a
multi-page title/grand-summary fixture and verifies complete report context.
Existing report tests supply the detailed content/fit/page-context coverage.

Primary references, consulted 2026-11-02:

- [Claris Print](https://help.claris.com/en/pro-help/content/print.html): dialog,
  stored printer/options, per-step printer scope. OpenMaker's refusal of missing
  requested printers is deliberately stricter than Claris's default fallback.
- [Claris printer and paper options](https://help.claris.com/en/pro-help/content/printer-paper-options.html):
  paper is per file; native printer preference behavior differs from this slice.
- [Claris error codes](https://help.claris.com/en/pro-help/content/error-codes.html):
  cancellation, unavailable, trigger veto, print and temporary-file failures.
- [CUPS lp](https://www.cups.org/doc/man-lp.html) and
  [lpstat](https://www.cups.org/doc/man-lpstat.html): submission, copies,
  destination discovery, `--`, and synchronous transfer to the scheduler.
- [CUPS printing options](https://openprinting.github.io/cups/doc/options.html) and
  [IPP implementation](https://www.cups.org/doc/spec-ipp.html): media,
  orientation, one page per sheet and physical-size output.
- [CUPS option encoder](https://github.com/OpenPrinting/cups/blob/master/cups/encode.c):
  `ipp-attribute-fidelity` is a boolean operation attribute; `page-ranges` is an
  IPP integer range.
- [CUPS lp source](https://github.com/OpenPrinting/cups/blob/master/systemv/lp.c):
  destination defaults are merged only when the option was not explicitly set.
- [OpenPrinting filter documentation](https://github.com/OpenPrinting/cups-filters/blob/master/README.md):
  `print-scaling=none` (also sent with `fit-to-page=false`).
- Repository evidence: `docs/re/script-steps.md` 11.0/11.6 and
  `crates/om-scripts/examples/stored_specs.rs` (tagged-stream preservation).
