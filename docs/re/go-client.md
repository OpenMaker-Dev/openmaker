# FileMaker Go client

How Claris documents FileMaker Go (iPhone and iPad), from its current help
and development guide (rebuilt August 2026; Go 26.1.1 is the release of
November 2026). Nothing here was measured against a running FileMaker Go.
Where the current help says nothing, the archived Go 15 help (about 2016)
is cited as such: its interface may have changed since.

Sources, abbreviated below:

- GH: <https://help.claris.com/en/go-help/content/> (FileMaker Go Help)
- DG: <https://help.claris.com/en/go-development-guide/content/> (Go
  Development Guide)
- PH: <https://help.claris.com/en/pro-help/content/> (FileMaker Pro Help)
- G15: <https://help.claris.com/archive/help/15/fmg/en/overview.html>

## Launch Center

- Tabs My Apps (files on the device, and FileMaker Cloud locations),
  Favorites, Recent and Hosts (local hosts, and hosts added with + by name
  or address, without `fmnet:/`). On iPad, Locations and Local Hosts are
  sidebars. Lists have a search box and refresh on a pull
  (GH connect-to-files).
- Touching and holding a file offers Rename, Delete, Favorite or
  Unfavorite, Remove (Recent), Share, Sign In and Open at Startup
  (GH files-and-hosts). Favorites can be local or hosted files.
- A host whose certificate cannot be verified, or that does not encrypt:
  Go may warn, offering Always Permit Connection (the host goes on the
  permitted hosts list), Connect (this time) and Don't Connect
  (GH manage-permitted-hosts). Lock icons on the opening screen and in the
  window title show unencrypted, encrypted but unverified, and verified
  connections (GH security-lock-icons).
- Files come in through the import icon, the Files app or the Finder
  (GH transfer-files). A public document type identifier for `.fmp12` is
  not documented; FileMaker Pro 26.1.1 for macOS declares
  `com.filemaker.doc.fmp12` (read from its Info.plist, [O]), and Go's own
  declaration is unverified.

## Browse

- The Actions menu: Launch Center, Layout (the layouts and View As Form,
  List or Table), Scripts, Print, Save/Send As, Export, Settings and
  Close File (GH filemaker-layouts-views, scripts, print-data,
  save-send-data, export-records).
- The Find icon: Quick Find (named for iPad), Create New Find, Modify Last
  Find, Constrain Found Set and Extend Found Set (GH search-records,
  modify-search).
- Records navigation: Show All, Show Omitted, a toggle between the found
  and omitted records, and Omit Records (the current record or a count)
  (GH search-records, modify-search). iPhone has one icon for Add Record,
  Delete Record, Delete Found Records and Sort Records; iPad has separate
  Add, Delete and Sort icons; a Table View column header sorts by its field
  (GH add-record, delete-record-found-set, sort-records).
- Windows show full screen; the window title adds, scrolls through and
  closes windows (GH manage-windows, PH new-window).
- Records: "Swipe with two fingers or tap the arrows to scroll through the
  records" (GH search-records). The direction is not stated; G15 says left
  and right, in Form View.
- How the record position is shown, Saved Finds (the step is "No" for
  Go: PH open-edit-saved-finds) and the bars' exact places are not
  documented. G15 had the menu bar (Actions, window title, Find) at the top
  and the toolbar (arrows, record count with a slider, add, delete, sort)
  at the bottom.
- The largest zoom is 200% (DG behavior-differences); how a user zooms is
  not documented. Pinch is documented only for container media
  (GH container-fields).

## Find mode

Create New Find (or Command-F) enters it. On iPad an icon adds a request;
the request icon (showing the number of requests) has Omit Matching and
Delete Find Request; arrows at the bottom move between requests; the
keyboard's Go key or Search at the top right performs the find
(GH search-records). Quick Find takes words; its only operator is `""`
(GH find-operators). No Cancel button is documented; Command-B returns to
Browse (GH keyboard-shortcuts-ios-ipados).

## Editing a record

- A record's changes are saved by Done on the editing toolbar, a tap
  outside every field, switching to Find mode, or changing the record,
  view or layout (GH work-with-records). The editing toolbar has Previous
  and Next (iPad) or arrows (iPhone), Insert (date, time, user name,
  barcode) and Revert Record (GH work-with-records, scan-barcodes).
- Auto-Correction is an app setting (GH work-with-records); it does not
  work in fields with keystroke triggers. The touch keyboard's type comes
  from the field (Inspector > Data > Behavior: System Default, Default for
  Data Type, ASCII, URL, Email, Numeric 10-Key, Number Keypad, Number and
  Punctuation, Phone) (PH display-keyboard).
- Only fields are in the tab order; Tab and Return move to the next
  field, Enter does not (DG behavior-differences).

## The app in the background

Go is suspended when the user switches apps (unless audio or video plays)
and resumes where it was; a script that was running stops if a hosted
file's data or schema changed meanwhile. Going to the background, Go
"saves the state of any open FileMaker apps"; if iOS ends it, a
hibernation file reopens the files on the next launch, unless a script
with Allow User Abort off was running (DG behavior-differences,
protecting-files). What becomes of a record's unsaved changes is not
documented. FileMaker Server's session timeout applies to Go clients
(<https://help.claris.com/en/server-help/content/config-client-timeouts.html>).

## Authoring and scripts

FileMaker Pro defines the schema, layouts, scripts, value lists and custom
menus; Go does not edit them, import Excel or XML, export FMP12 or XML,
use plug-ins or host files, and has no Layout or Preview mode
(DG go-pro-differences, behavior-differences). A script step Go does not
support is skipped and sets the last error to 3. Each step's compatibility
table in PH script-steps-reference says "No" or "Partial" for Go: no
Spelling steps, 13 of the 16 Open Menu Item steps, file administration,
Enter Preview Mode, Perform Find/Replace, Insert from Index, Save Records
as Excel or JSONL, and the plug-in and operating system steps; Print,
PDF, Import, Export, window size and position, Set Zoom Level, Show Custom
Dialog, Send Mail and Exit Application are partial. Custom menus cannot
remove Close File, add items with no Go counterpart, or override Quick
Find (DG behavior-differences).

## Unknowns

- Layout scale, font metrics and line breaking on the devices.
- The toolbars' geometry on iPhone and iPad, the record indicator, and
  whether pinch zooms a layout.
- What Go does with a record's unsaved changes when it is suspended or
  ended.
- Go's document type declaration for `.fmp12`.
