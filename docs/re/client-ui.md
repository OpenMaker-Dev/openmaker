# FileMaker Pro client UI: application chrome

FileMaker Pro 26.0.3's application chrome on macOS: the window, toolbars,
menus, modes and dialogs around a file's layouts. Distilled from the captures
in `re/captures/fm-ui/` (its `INDEX.md` has the full measured breakdown).
Coordinates are screenshot pixels from capture 10 unless noted; the document
window there starts at x=66, y=30 and is 1060 px wide. What OpenMaker builds
from this, and where it differs, is in
[desktop-client.md](../desktop-client.md).

## Chrome vs content

FileMaker behaves like a browser. Everything above the content boundary, plus
menus and dialogs, is **application chrome**, the same for every file.
Everything below the layout bar is the **file's rendered layout**: object
geometry, theme colors, fonts, tabs, buttons and "navigation-looking" links all
come from the fmp12 file (see `layout-model.md`), not from the application.

- Browse/Find/Preview: chrome ends at y=135 (window-relative 105).
- Layout mode: chrome ends at y=129, then authoring overlays (pasteboard, part
  labels, object outlines, Inspector) surround the file's design.
- There is no bottom status bar in FileMaker 26 on macOS.

## Window regions (top to bottom)

| Region | Height | Background | Notes |
| --- | --- | --- | --- |
| Menu bar | ~24 px rows | system (macOS) | The macOS menu bar, outside the window. |
| Title | y=30-56 | `#F4F4F4` | Unified with the toolbar; bold centered title `<file> (<host>)`. |
| Status toolbar | y=56-110 (54 px) | `#F4F4F4` | Icons above 11 px captions. |
| Layout bar | y=110-135 (25 px) | `#DEDEDF` | 1 px top rule `#B8B8B9`, 1 px bottom rule `#AEAEAE`. |
| Content | from y=135 | file theme | Rendered layout. |

## Status toolbar (Browse), left to right

In FileMaker Pro 26.1.1, Show/Hide Toolbars [Hide] removes both the
status toolbar and the layout bar. The same step in OnFirstWindowOpen
gives the same view as running it after open. Lock disables View >
Status Toolbar and its Option-Command-S shortcut whether the bars are
shown or hidden. A Show with Lock off restores manual control, even if
the bars were already shown. The native steps all returned LastError 0.
Toggle with Lock and new-window inheritance were not measured.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1040#issuecomment-7188552787)

1. **Record navigation group**, captioned **Records** (caption centered under
   the group):
   - Previous / Next chevrons (thin gray, ~16 px tall) separated by a hairline.
   - Current-record number box: white, `#C5C5C5` border, ~71 x 14 px, gray
     centered number. Accessibility: the slider is *Current Record*.
   - Record slider directly under the box (same width, small knob).
   - Found-set ring: ~20 px circle, 2.5 px stroke `#7AAFE6` when the found set
     is every record (*Percent in Found Set*). A subset is shown as a darker
     pie wedge.
   - Count text, two lines at ~11 px: found count, then `Total (Unsorted)` /
     `Total (Sorted)`.
2. **Show All** (disabled when nothing is omitted), **New Record**,
   **Delete Record**: icon-above-caption buttons, ~80 px pitch.
3. **Find** (split: magnifier + hairline + options chevron), **Sort**,
   **Share** (paper plane + chevron, opens a menu).
4. **Quick Find** search field filling the remaining width: 22 px tall,
   rounded, fill `#EEEEEE`, magnifier-with-chevron glyph, placeholder
   `Search`.

Icons are monochrome line glyphs in `#7D7D7D`; disabled icons and captions are
pale (`#C8C8C8`); captions are `#5A5A5A`-ish. Selected/active states are blue.

## Layout bar (Browse), left to right

- `Layout:` label + popup (~132 x 16 px, 4 px radius, `#9B9B9C` border, chevron
  at right). The popup is a menu with a checkmark on the current layout;
  layouts may be grouped in folders (Contacts > Contact List / Contact Details).
- Hairline divider.
- `View As:` + Form / List / Table glyphs ~24 px apart. Selected glyph
  `#2F54E5`; available `#A5A5A6`; unavailable light gray. Available views are
  constrained per layout (a layout may forbid Table).
- Hairline divider, **Preview** button (small white rounded rect).
- Right aligned: `Aᵃ` Show/Hide Text Formatting toggle, **Edit Layout** button.

## Modes

Mode is indicated by the checked View menu item and by the toolbar context;
there is no permanent mode badge.

| Mode | Toolbar | Layout bar | Content |
| --- | --- | --- | --- |
| Browse | Records group, Show All, New/Delete Record, Find, Sort, Share, Quick Find | Layout, View As, Preview, Aᵃ, Edit Layout | Records rendered by the layout. |
| Find | **Find Requests** group (count + `Total`), New Request, Delete Request, Perform Find, Cancel Find; Saved Finds at right | Layout, View As, `Matching Records:` Include/Omit segmented control, `Insert:` Operators popup | Layout fields as empty criteria inputs with find glyphs. Records menu becomes **Requests**. |
| Layout | Objects, New Layout / Report; centered Selection (blue filled when active), Text, Shape, Field, Button, Panel Tools, Portal, Chart, Web Viewer, Format Painter, Part Tool; right Manage, Inspector | Layout popup + setup pencil, `Table: <name>`; right `Theme: <name>`, guides, Aᵃ, Exit Layout | Pasteboard `#E5E5E5`, vertical part labels at left, design surface, Inspector (~252 px, white) at right. Menu bar swaps Records for **Layouts** and adds **Arrange**. |
| Preview | **Pages** group, Save as Excel, Save as PDF, Print, Page Setup | Layout, View As, Exit Preview | Gray print workspace with a white page and dotted printable bounds. |

Shortcuts (macOS): Browse ⌘B, Find ⌘F, Layout ⌘L, Preview ⌘U.

### Find mode content

What the window shows in Find mode, with its evidence:

- The current layout itself, its fields empty for criteria [O, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201):
  FileMaker Pro 26.0.3 kept the designed layout in Find mode on the
  starter solutions it was compared on]. A related field in a portal takes
  criteria, and finds "all the records that have a related record matching
  the criteria you enter" [Claris: FileMaker Pro help, "Making a find
  request"].
- A magnifier badge marks the fields of Find mode; "it does not appear in
  check boxes, pop-up menus, or radio buttons". Container, summary and
  global fields and web viewers take no criteria [Claris, same page]. Where
  the badge sits in the field is not recorded here [U].
- Entering Find mode puts the focus in the first field of the layout's tab
  order [O: FileMaker Server's script engine, s33; layout-model.md 5.5,
  `KTabOrder`]. Where it goes when that field takes no criteria (a global
  or container field, or one whose Find mode entry is not Edit) is [U];
  OpenMaker takes the next one that does.
- A field object's placeholder shows unless its field flag `0x10000` is
  set (layout-model.md 5.4.1) [O, third party].
- A hide condition applies in Find mode only with "Apply in Find mode"
  (layout-model.md 5.5.2).
- Unmeasured [U]: whether a portal shows one row or all of its rows (and
  which row takes criteria), what merge fields and calculated labels show
  with no record, whether Return and Enter perform the find, how List and
  Table view show several requests, and how pop-up menus, checkbox and
  radio button sets, drop-down lists and calendars build criteria (which
  items a list of related values offers with no record, what several
  checked boxes find). Claris's note that the badge "does not appear in
  check boxes, pop-up menus, or radio buttons" implies those controls
  stay controls in Find mode [I].

### Tab in Browse [O, 2026-11-14, #848]

FileMaker Pro 26.0.3 was observed on a local Contacts starter copy, by its blue focus outline and macOS accessibility focus.

**Contact Details, Form view.** Tab moves through the layout's stored tab order (calc-functions.md 6.1, "Tab order"), on the record, in an 11-stop cycle. After New Record:
1. Title, the pop-up menu;
2. First Name;
3. Last Name;
4. Job Title, the drop-down list;
5. the front tab panel, Company, itself (the panel does not change);
6. Company, a pop-up menu;
7. Website;
8. and 9. two stops in the web viewer, its area and its wrapper;
10. the Contacts navigation button;
11. the Photo container.

Then Title again. The cycle repeated over 22 more Tabs, and the hidden tab panels never came forward.

**Other observations:**
- **Shift-Tab** goes back the same way.
- **Typing** after a Tab reaches the field entered.
- **Tab into an edit box that holds text** selects nothing: the insertion point is after the text.
- **With a pop-up menu's list open** and an item highlighted, Tab leaves the list open, chooses nothing and does not move the focus. Escape dismisses the list.

**Contact List, List view.** Tab goes First Name, Last Name, Company, the row's Details button, the row's photo container, then First Name of the same row. It never moves to another record.

**Edit boxes excluded from the order.** In a local synthetic Form layout
on FileMaker Pro 26.1.1, three ordinary edit boxes remained clickable
with a two-panel tab control as the only saved stop. Tab and Shift-Tab
from an excluded edit box focused the selected panel and kept the field's
change pending. Typing with that panel focused changed no field. Another
Tab left the panel focused. Clicking the Body background committed the
edited fields. With the three fields included and the control excluded,
Tab advanced through the fields, preserving their pending changes until
the background click. Portal and List view cases were not measured.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/253).

**Tasks starter, Task Details** (a local copy, the research thread on #848, 09:34Z):
- **Find mode.** From the Task criterion, Tab went Due Date, Category, Priority, Status, Completion Date, Assignee Name, Assignee Email, the attachment portal's Description, then the main Description. That is the same field order as Browse for the fields both show. Buttons, portal controls included, were stops between some of them. After the main Description, Tab reached the Tasks navigation button, then wrapped to Task. The request was not performed.
- **Table view**, turned on for Task Details in Layout Setup on the copy, since the starter disables it. From the first record's Task cell, Tab moved across the same row through its focusable cells, passing over empty related-field columns. From the last cell it returned to Task in the same row. It never entered the next record, and the record indicator stayed `1 of 3`.
- **A portal row** (the attachment portal). From a row's Description, Tab went:
  1. to that row's delete button;
  2. then its attachment button;
  3. then the next row's Description.

  It went on through the blank (creation) row and its buttons, then to the main Description below the portal: row by row, at the portal's place in the tab order.

  **Shift-Tab** [O, the research thread on #848, 10:07Z, the same copy]. From the second, blank row's Description, it went:
  1. to the first row's attachment button;
  2. then its delete button;
  3. then the first row's Description.

  That is the Tab order reversed, buttons included, crossing to the row before.

**Shift-Tab on Task Details** (the same copy, the same 10:07Z comment; observed for that layout only):
- **Table view.** From the first record's Task cell: the main Description cell of the same row, then the attachment Description cell, then the Completion Date cell. It wraps backward within the row and never goes to the record before.
- **Find mode.** From the attachment Description criterion: the Add Assignee button, the Assignee delete button, then the Assignee Email criterion. From the Task criterion: the Tasks navigation button, then a wrap to the main Description criterion. That is the Tab order reversed, buttons included, wrapping at the start.

**A drop-down calendar** [O, #964, the Meetings starter, a local copy]. On Meeting Details, after New Record, a title typed and Tab, a location typed and Tab, the focus is in Date, a drop-down calendar with its icon holding the auto-entered current date. Typing `11/20/2026` there leaves `11/20/2026`: the date was selected, unlike an edit box's text above. The two readings differ in the control (calendar or edit box), the field's type (date or text) and how its value came (auto-entered or typed), so which of these decides is [U].

**Not measured:** a portal row with several fields; a custom tab order's effect in Table view; a field whose "Go to next object using" has Tab turned off; Tab with no field focused; Tab into a drop-down list, pop-up menu or checkbox set that holds a value, or into a calendar in Find mode or in a portal row.

### Editing a record, and Revert Record... [O, 2026-11-14, #871]

FileMaker Pro 26.0.3, local copies of the Tasks and Contacts starters (#871, 10:31Z and 12:19Z).

**No bar.** A record with changes not committed (New Record on Task Details, then `A` typed into Task) shows no bar or overlay over the layout, and the layout does not move.

**When the menu item is enabled:**

| State | Menu item |
|---|---|
| Existing record, no change | Records > Revert Record... disabled |
| Existing record, a field retyped, not committed | Revert Record... enabled |
| A portal row's field edited (the first Address row's Address Line 1) | Revert Record... enabled |
| Right after Records > New Record, nothing typed (3 of 3) | Revert Record... enabled |
| Find mode, an empty new request | Requests > Revert Request... disabled |
| Find mode, a criterion typed | Revert Request... enabled |

Neither item has a shortcut (`AXMenuItemCmdChar` absent).

**The alert.** For an edited record and for an untouched new one alike: no title, the text "Revert all changes to this record since it was last entered?", and the buttons Cancel, then Revert. Neither is marked as the default: Return leaves the alert open, and Escape cancels it and keeps the changes. Revert Request... asks "Revert all changes to this find request since it was last entered?" with the same buttons.

**What Revert does:**
- an edited record: the old values come back, the record stays current, and no field keeps the caret or the blue focus;
- an edited portal row: that row's text comes back, and the other rows stay in place;
- an untouched new record: it is removed (3 of 3 became 2 of 2), and the former last record is current;
- a find request: the typed criterion is cleared.

**Not measured:** the alert of the Revert Record/Request script step with its dialog; keypad Enter as a commit beyond the one ordinary Text field of script-runtime.md 7.1 (Claris Help, as #871 cites it).

## Menus

Pale rounded panels with a soft shadow, ~24 px rows, a left checkmark column,
a right shortcut column in gray, submenu chevrons, hairline separators.
Hovered rows get a rounded blue fill with white text. Disabled rows are pale
gray.

- **App menu** (FileMaker Pro): About,
  Settings, Quit.
- **File**: Create New... | Sign In to Claris ID, Favorites >, Recent >,
  Hosts >, Open... ⌘O, Open Quickly... ⌘K, Close ⌘W | Manage > (Database,
  Security, Value Lists, Layouts, Scripts, External Data Sources, Containers,
  Custom Functions, Custom Menus, Themes) | Sharing > | File Options...,
  Change Password... | Page Setup..., Print... ⌘P | Import Records >, Export
  Records..., Save/Send Records As >, Send >, Save a Copy As..., Recover...
- **Edit**: Undo ⌘Z, Redo ⇧⌘Z | Cut, Copy, Paste, Paste Text Only, Clear |
  Select All | Find/Replace >, Spelling > | Export Field Contents...
- **View**: Browse/Find/Layout/Preview Mode | Go to Layout > | View as Form /
  List / Table | Status Toolbar ⌥⌘S, Customize Status Toolbar...,
  Formatting Bar | Ruler | Actual Size, Zoom In, Zoom Out.
- **Insert**: Picture..., Audio/Video..., PDF..., File... | Current Date,
  Current Time, Current User Name | From Index..., From Last Visited Record.
- **Format**: Font >, Size >, Style >, Align Text >, Line Spacing >,
  Text Color >.
- **Records** (Browse/Preview): New Record ⌘N, Duplicate Record ⌘D, Delete
  Record... ⌘E | Delete All Records... | Go to Record >, Refresh Window ⇧⌘R |
  Show All Records ⌘J, Show Omitted Only, Omit Record ⌘T, Omit Multiple...
  ⇧⌘T | Modify Last Find ⌘R, Saved Finds > | Sort Records... ⌘S, Unsort |
  Replace Field Contents... ⌘=, Relookup Field Contents | Revert Record...
- **Requests** (Find): Add New Request, Duplicate Request, Delete Request |
  Show All Records | Perform Find, Constrain Found Set, Extend Found Set |
  Revert Request...
- **Layouts** / **Arrange** (Layout mode): layout management and object
  arrangement commands.
- **Scripts**: Script Workspace... ⇧⌘S (plus the file's scripts).
- **Tools**: Script Debugger, Debugging Controls >, Data Viewer | Custom
  Menus > | Save a Copy as XML..., Database Design Report..., Developer
  Utilities..., Tools Marketplace, Create Add-on...
- **Window**: New Window, Show Window >, Hide Window, Minimize Window ⌘M |
  Tile Horizontally, Tile Vertically, Cascade Windows, Bring All To Front |
  ✓ current window title. Claris documents Hide Window for hiding a
  document window and Show Window > window name for showing one that was
  hidden
  ([FileMaker Pro 12 Help, "Arranging windows"](https://help.claris.com/archive/help/12/fmp/en/html/fmp_basics.3.30.html)).
  [I] Show Window lists every open window, a hidden one's name in
  parentheses, and choosing one also brings it to the front; not measured
  in Pro 26, nor its order, its check mark, or whether reduced menus keep
  it ([#878](https://github.com/OpenMaker-Dev/openmaker/issues/878)).
- **Help**: search, FileMaker Pro Help, Keyboard Shortcuts | documentation and
  support links. Plug-in items (MBS) in the captures are not baseline chrome.

## Home window

[O: FileMaker Pro 26.1.1 on macOS in the light appearance with the default
accent color: the Home window, 1101 by 729 pt, showing Recent, captured at
2x while the window was inactive, and its accessibility tree.] Positions are
points from the top-left of the window's content, below the title bar.

- **Header.** A white band 51 pt tall with the Claris FileMaker Pro logo at
  x 22, over a 1 pt `#E6E6E6` rule.
- **Sidebar.** 198 pt wide and white, with a 1 pt `#E6E6E6` rule on its
  right edge. Create and Open are capsule buttons 144 by 28 pt at x 25,
  y 84 and 124, filled `#ECECEC`, with 13 pt near-black labels. A
  full-width `#E6E6E6` rule at y 179.5 separates them from the pages: Home,
  FileMaker Cloud and Resources, in rows 156 by 30 pt from x 20, 38 pt
  apart from y 198, each a 17 pt gray glyph at x 31.5 and 13 pt gray text
  (about `#737373`) at x 58.5. The selected page (Home) keeps its gray text
  on a `#E9E9EB` fill with corners of about 5 pt.
- **Page.** Its title, Home, is at x 227.5 in bold of about 19 pt (cap
  height 13.5 pt). The Favorites and Recent tabs are 13 pt gray text (about
  `#767676`) at x 220 and 288.5; the selected one differs only by a 2 pt
  `#2D55E7` underline at y 152.
- **Table.** It runs from x 211 to 38 pt short of the window's right edge,
  with no frame, stripes or row rules. The header's Name and Location are
  11 pt near-black, with `#E5E5E5` column rules from y 167.5 down to the
  header's bottom rule at y 189, at x 463 and 13 pt before the table's end.
  Rows are 50 pt tall from y 194.5: a 32 pt icon at x 230 (the solution's
  own icon, or FileMaker's logo for a hosted file), the name at x 270.5 and
  the location at x 474, both 13 pt near-black, the location cut with an
  ellipsis. A file on this computer shows its full path, starting with
  `/Volumes/` and the volume's name; a hosted file shows its host alone.
- **Rows** are selectable, and each advertises the accessibility actions
  Add to Favorites and Remove.

Claris documents the commands
([favorite files](https://help.claris.com/en/pro-help/content/favorite-files-hosts.html),
[opening and managing files](https://help.claris.com/en/pro-help/content/opening-managing-files.html)):
File > Favorites > Show Favorites and File > Recent > Show Recent show the
lists, where a file opens with a double-click; both submenus also list the
files. File > Favorites > Add Current File to Favorites pins the open file.
A favorite is removed by right-clicking it and choosing Remove, or selecting
it and pressing Delete; a recent file by right-clicking it and choosing
Remove. Neither deletes the file. The macOS startup window restores the last
tab ([release notes](https://help.claris.com/en/pro-release-notes/content/)).

Unknowns: the active window's look (the capture is of an inactive window,
whose title and window buttons are gray, so the gray page selection may be
macOS's inactive style), a selected row, Favorites rows' actions beyond
Remove, an empty list, the order of the File > Favorites and File > Recent
submenus, the history's length and order, the FileMaker Cloud and Resources
pages, and Windows.

## Dialogs

- Buttons: compact white rounded rectangles ~22 px tall; the default action is
  a saturated blue/indigo fill with white label. Focus rings are blue-violet.
- **Hosts** (capture 28, ~828 x 540): left sidebar `#E3E3E4` with Search
  Hosts, a `+` add-host button, Favorites and Local sections (selected host is
  a blue rounded row); right pane with the host heading, tile/list toggles and
  Search Files, a white file area, and Cancel / OK at the lower right.
- Authoring dialogs (Manage Database, Script Workspace, Security, Value
  Lists, Themes, Custom Functions, External Data Sources, Calculation, Data
  Viewer, Inspector panels) are application chrome whose lists are populated
  from file metadata; INDEX.md "Authoring views to reproduce" lists the
  captures. Specify Calculation is measured below.
- **Delete confirmations** [O: FileMaker Pro 26.0.3, a local Tasks
  starter copy, the alerts' accessibility text]. Records > Delete
  Record... with no portal row selected shows a critical alert, its window
  named `FileMaker Pro`, with the message "Permanently delete this ENTIRE
  record?" and the buttons Cancel and Delete. The attachment row's delete
  control, a one-step Delete Portal Row button with its dialog, shows
  "Permanently delete this one related record?" in the same alert. With a
  row of that portal selected (it does not allow deleting its records),
  both Delete Record items and the toolbar's Delete Record are disabled.
  The buttons' left-to-right order, the default button, the wording for a
  hosted file and for a selected row of a portal that allows deleting are
  [U].
- **Sort Records** (Browse: the toolbar's Sort, Records > Sort Records...,
  Sort Records with a dialog). The dialog script-steps.md ("Sort order")
  describes for the Script Workspace, with a table list that offers
  "Current Layout (LayoutName)" (the layout's fields) and "Current Table
  (TableName)" besides related tables, fields moved into the Sort Order
  with Move, an entry taken out by a double click, a related field sorted
  by "the value in the first matching record", and an **Unsort** button
  that restores creation order, as Records > Unsort does; Sort sorts
  [Doc: Claris help `sorting-records-options.html`, `sorting-records.html`].
  [O: FileMaker Pro 26.0.3, a local Tasks starter copy, #840, 2026-11-14]:
  - It opens on `Current Layout ("Task List")`, whose list is the layout's
    fields in alphabetical order (Category, Due Date, Status, Task), not
    their object order. On Task Details it also lists related fields
    (`Assignees::Email`, `Assignees::Name`).
  - The table pop-up: `Current Layout ("…")`, `Current Table ("Tasks")`
    (the table's fields in schema order: Task, Due Date, Description,
    Status, …), the related tables (Assignees, Assignments, Attachments),
    a disabled unrelated-tables heading with `<none>`, and Manage
    Database....
  - The starter's Status sort shows as "Custom order based on value list"
    with the Status list; that pop-up offers the file's lists and Manage
    Value Lists.... "Keep records in sorted order" is checked.
  - Unsort is enabled whether or not the found set is sorted. After
    Unsort the dialog still shows the order last sorted by.
  - The last sort order is the table's, within a window. Tasks sorted by
    Task descending and Assignees by First Name descending each came back
    in the dialog after switching between their layouts with the layout
    pop-up. Whether two windows on one table share it is unmeasured.

  Still [U]: the button row's order and default, whether Sort is enabled
  with an empty order, what a script's Sort Records [With dialog] does on
  Unsort, and where related fields fall in Current Layout's alphabetical
  list.
- **No records match** (error 401). A find the user runs from the window
  (Find mode's Perform Find, Constrain or Extend Found Set) that matches
  nothing shows "No records match this find criteria." with **Modify
  Find**, which goes back to Find mode with the requests as they were, and
  **Cancel**, which goes back to Browse with the found set as it was before
  the find, "as though you'd never performed a find" [Reported: O'Reilly,
  *FileMaker Pro 9: The Missing Manual*, "Navigating Your Database"; The
  Support Group, "Quick Find, Error Capture, and Omit All"; Philosophy of
  FileMaker, "Ultimate Find". Unmeasured in FileMaker Pro 26 here]. A
  Quick Find that matches nothing shows the same message with OK and keeps
  the found set (query-program.md 6.6). A script's Perform Find without
  error capture is reported to offer Cancel, Continue and Modify Find and
  to end with no records [Reported: FMForums, "No records match this
  request"; unmeasured]. Which button is the default, the dialog's title,
  and whether a one-step button's find counts as the user's are [U];
  OpenMaker makes Modify Find the default and treats a step the window
  runs (a menu, the toolbar, a one-step button), even over a paused
  script, as the user's; a trigger's script is a script.

## Typography and metrics

- System sans (SF Pro on macOS), ~11 px for toolbar captions and layout-bar
  text, ~13 px for menus and dialog text, bold window title.
- Menu rows ~24 px; dialog buttons ~22 px; layout bar 25 px; toolbar 54 px.
- Colors: toolbar `#F4F4F4`, layout bar `#DEDEDF`, pasteboard `#E5E5E5`,
  Inspector `#FFFFFF`, Hosts sidebar `#E3E3E4`, selected view glyph `#2F54E5`,
  found-set ring `#7AAFE6`, icon gray `#7D7D7D`.

## Layout mode and Manage Layouts resources (2026-10-28)

### Reference evidence

The installed FileMaker Pro executable SHA-256 is
`691caa842c83d2791573c983ca496d975b3dfe9798e2c728d1fb77db9f65d70f`.
Computer Use observed the hosted document and its View menu, including the four
modes, layout submenu, view choices, toolbar and formatting commands. Document
controls reported disabled, and attempted mode changes did not produce a new
screen; this pass does not establish runtime authoring behavior.

REA decoded eight selected compiled Interface Builder archives copied unchanged
from the installed application's `Contents/Resources/Base.lproj/` into a
temporary analysis bundle with the identical executable. No vendor resources
are included in OpenMaker. Retained parent Evidence:
`ev_1834ced90ae144b61d8896ad0191d7ce9a55d98dda0361b32af5443ed724587b`.
The initial full-bundle scan hit the 64-document limit before these authoring
resources; only this focused follow-up supports the labels below.

Observed serialized labels (static evidence, not proof of runtime visibility):

- `LayoutLeftPanel.nib`: Fields, Objects, Add-ons.
- `LayoutObjectTree.nib`: Layout Objects, Search, Filter, Show all; the search
  help says it searches objects displayed in the list.
- `PositionInspector.nib`: Position, Left, Top, Right, Bottom, Width, Height,
  Name, Tooltip. Original archive SHA-256:
  `0d760f4b9a051716062f050bceb1abaa639a5e0e779279eefb8606bbf925b07f`.
- `ArrangeAlignInspector.nib`: Arrange & Align, Arrange, Group, Lock, Align,
  Space, Resize, with action labels for ordering, alignment and distribution.
- `AutosizingInspector.nib`: Autosizing, Autoresize, four edge anchors.
- `ManageLayouts.nib`: Layout Name, Associated Table, Menu Set; New, Edit,
  Duplicate, Delete, Open, Find, Show All, Include in layout menus. Original
  archive SHA-256:
  `93fdb5a507dd06eb4d3c6c5379db0ceb74cd9d1adaabfe92b6eafe36ba531552`.

The decoder recognizes only part of the serialized object graph. Constraints,
actual pixel layout, dynamic controls, runtime handlers and some hierarchy links
remain unknown, so OpenMaker's Layout mode dimensions and interactions are its
own choices ([desktop-client.md](../desktop-client.md), Layout mode).

## Layout mode: Selecting and moving objects

Claris's [Editing objects, layout parts, and the layout background](https://help.claris.com/en/pro-help/content/editing-objects-parts-background.html)
documents Edit > Select All and dragging a selection box with the Selection
tool. The box can cross an object to select it. Holding Command on macOS or
Control on Windows restricts selection to objects entirely inside the box.
Dragging on an open popover selects within the popover. Claris also documents
tool-specific Select All and selecting objects of the same type.
[Moving objects on a layout](https://help.claris.com/en/pro-help/content/moving-objects.html)
documents moving the selection by one point per arrow key press. See
[layout-model.md 10.7](layout-model.md#107-an-objects-position) for how a
move is stored.

**Unknowns:** Which nested objects Select All and a layout-level box select,
how they treat locked objects, and whether a modifier changes the arrow
step. These gestures were not observed in a running FileMaker Pro window.

### Front panels

The [panel selection help](https://help.claris.com/en/pro-help/content/objects-on-panel-controls.html)
describes one front panel in Layout mode. Clicking another tab or slide dot
brings that panel to the front. It also describes selecting objects across
panels with Shift, and selecting all panels and their objects with a box
that surrounds a whole control. The panels have their own stacking order.
The [tab control help](https://help.claris.com/en/pro-help/content/changing-tab-control.html)
says a tab control keeps the previously displayed tab when entering Layout
mode. The [slide control help](https://help.claris.com/en/pro-help/content/changing-slide-control.html)
identifies the leftmost slide as the default.

**Unknowns:** whether selecting an object in the Objects pane switches its
containing panel or changes the current selection. The [Objects pane
help](https://help.claris.com/en/pro-help/content/working-with-layout-objects.html)
describes the object list and its temporary hiding tools, including that
the pane cannot hide or reorder panels. These behaviors were not measured
in a running FileMaker Pro window.

## Layout mode: Align, Distribute and Resize To

From Claris's help (doc), fetched 2026-11-20:
- The FileMaker Pro 2026 page "Arranging objects" (`arranging-objects.html`)
  names the Inspector's Align and Space controls, and says those commands
  are also in the Arrange menu and the shortcut menu. It does not give
  their rules.
- The FileMaker Pro 2026 page "Resizing and reshaping objects"
  (`resizing-reshaping-objects.html`) documents the Resize controls.
- The Align and Distribute rules are from "Aligning or distributing
  objects", FileMaker Pro 12 (`archive/help/12/fmp/en/html/edit_layout.10.50.html`).
- The notes on groups and repetitions are from "Resizing and reshaping
  objects", FileMaker Pro 12 (`archive/help/12/fmp/en/html/edit_layout.10.8.html`).

`ArrangeAlignInspector.nib` carries the Align, Space and Resize labels
(above). Nothing here was compared with a FileMaker Pro screen.

- **Align**:
  - left edges, vertical centers, right edges, top edges, horizontal centers
    and bottom edges (doc);
  - "When you align objects along their tops, bottoms, or sides, the
    objects align with the object farthest out of alignment" (doc): left
    edges to the leftmost left edge, right edges to the rightmost right
    edge, top edges to the topmost, bottom edges to the bottommost (inf);
  - "To force the alignment to a specific object, lock that object before
    you set the alignment" (doc);
  - which line centers align to is not stated (U).
- **Distribute** horizontally or vertically: "FileMaker Pro takes the space
  separating the two objects farthest apart and divides it evenly among all
  the selected objects" (doc). That the two outermost objects stay and the
  others move so the gaps between neighbours are equal is inferred. When
  the objects are wider (taller) together than the space between the
  outermost edges, equal gaps would be negative and could reverse the
  objects' order. OpenMaker then spaces their leading edges evenly instead
  (its choice; FileMaker Pro's is unmeasured).
- **Resize**: match all widths, all heights, or both to the smallest or the
  largest selected object, in one step (doc).
- **Notes** (doc): resizing a grouped object resizes every object in it, and
  resizing a repeating field's first repetition resizes all of them.
- **Unknowns**:
  - the Arrange menu's submenu item names and order, and their shortcuts;
  - which objects take part when the selection holds a locked object, a
    group, or objects in a panel or portal;
  - how a fractional result is rounded. OpenMaker puts left and top edges
    on whole points, so aligning centers again can move an object by one
    more point;
  - whether Distribute needs three objects.

## Fields in the Layout sidebar

Claris' public Help documents the Fields tab in Layout mode as a list of
field definitions for the current layout's table, with a field-name search
and a table selector ([Fields tab](https://help.claris.com/en/pro-help/content/defining-fields-fields-tab.html)).
Dragging a listed field onto the layout places it; the Field tool instead
asks for its binding in Specify Field ([placing fields](https://help.claris.com/archive/fm22/en/pro-help/content/placing-removing-fields.html)).
These are documented behaviors, not a fresh native measurement. The owner's
#1238 report distinguishes an empty Objects list on a blank layout from
Manage Database's existing field definitions.

A field definition is identified by its table occurrence, base table and
field IDs, not its name or field ID alone: different tables can reuse IDs.
The sidebar's definitions and the layout's already-placed objects are
separate inventories. The owner's deferred Save workflow below also applies
to fields dragged from the list. Exact native drag preferences, default
frame size and schema-refresh timing have not been measured for this slice.

## Selecting several layout objects

Claris Help documents Shift-click to add individual objects to a selection
and Shift-click on selected objects to remove them. An ordinary click
selects one object; a blank-area click deselects the selection
([selecting objects](https://help.claris.com/en/pro-help/content/editing-objects-parts-background.html)).
Dragging a selection moves its objects together
([moving objects](https://help.claris.com/en/pro-help/content/moving-objects.html)).
These are public documented behaviors, also reported by the owner in #1241,
not a new native measurement. They apply while layout changes are unsaved.

OpenMaker's supported multi-selection operations and Inspector restrictions
are implementation choices described in the desktop feature note. Native
mixed locked-object behavior, multiple-object Inspector values, grouped
objects and selection-box geometry remain unmeasured in this slice.

## Completing a field placement

Claris' public Help documents drawing a field and choosing its binding in
Specify Field as a placement, and replacing an existing field as a separate
double-click action ([placing and removing fields](https://help.claris.com/archive/fm22/en/pro-help/content/placing-removing-fields.html)).
Its [layout tools](https://help.claris.com/en/pro-help/content/layout-tools.html)
documentation distinguishes clicking a tool for one use from double-clicking
to lock it for repeated use. These are documented behaviors, not a new native
FileMaker measurement. Consecutive completed placements therefore represent
independent objects; interpreting a second placement as a replacement of an
unfinished field's frame is not supported by this evidence.

The owner's #1240 clarification establishes that completing a placement
adds an object to the unsaved layout: developers can continue editing and
discard all changes before Save. Completion must not persist an individual
object. A completed field keeps its own binding and frame in that draft.
The public Help's tool lifecycle is documented evidence; this deferred-save
workflow is owner-reported evidence, without a fresh native measurement.
Exact native wire save timing, cancellation after a host refusal and the
locked-tool preference have not been measured for this slice.

## Layout Setup

REA decoded `IDD_LAYOUT_SETUP.nib` from the installed application's
`Contents/Resources/Base.lproj/`, copied unchanged with the executable
into a temporary analysis bundle (archive SHA-256
`b70b2cc22aa90f58b3218d3dc1731e50620f35e7302714a03d9fb3c21f38c09e`). Static
evidence; runtime visibility and pixel rendering are not observed.

- **Window:** "Layout Setup", content 485 x 428 pt. A tab view at
  `{13, 40}` 467 x 376 (AppKit coordinates, origin bottom left); each tab's
  content at `{10, 33}` 447 x 330 in it. Cancel `{287, 12}` and OK
  `{383, 12}`, 96 x 32 (bezel included).
- **Tabs:** General, Views, Printing, Script Triggers.
- [O, 2026-11-14, FileMaker Pro 26.0.3, a 2x capture] The window is about
  485 x 456 pt with its title bar, on both tabs; it opens on General with
  the Layout Name's text selected. Properties… is enabled; Reset Quick
  Find is disabled while Quick Find is unchanged.
- **General** (top to bottom): "Layout Name" over a 356 pt text field;
  "Include in layout menus"; "Show records from:" and a pop-up menu; "Save
  record changes automatically"; "Show field frames when record is
  active"; "Delineate fields on current record only"; "Show current record
  indicator in List View"; "Menu Set:" and a pop-up menu; "Enable Quick
  Find" beside a "Reset Quick Find" button.
- **Views:** "Views available in Browse mode and Find mode:"; Form View,
  List View and Table View checkboxes, each beside a 48 x 48 picture;
  "Properties…" beside Table View; "(At least one view must be selected)";
  "Default view:" and a pop-up menu.
- **Printing:** "Print in" n "columns", "Across first" and "Down first";
  "Use fixed page margins" with Top, Bottom, Left and Right, and "Facing
  Pages". [O, static: the strings of FileMaker Pro 26.1.1's compiled nib;
  AppKit frames in a tab view 339 high] "Print in" `{61, 265}` 78 x 18, its
  field `{138, 264}` 42 x 22, "columns" `{187, 266}`; "Across first"
  `{121, 225}` and "Down first" `{279, 225}`, beside the pictures
  `IDB_LAY_OPT_ACROSS` `{67, 210}` and `IDB_LAY_OPT_DOWN` `{225, 210}`,
  48 x 48; "Use fixed page margins" `{61, 165}` 180 x 18; "Top:"
  `{78, 138}` and "Bottom:" `{74, 112}`, their fields `{136, 136}` and
  `{136, 110}`; "Left:" `{227, 138}` and "Right:" `{227, 112}`, their
  fields `{292, 136}` and `{292, 110}`, each field 60 x 22 with a 29 x 17
  unit label 5 points after it; "Facing Pages" `{292, 84}` 106 x 18.
- **Script Triggers:** the event and script list, and "Script Trigger
  Properties" (Event, Script, Select…, "Enable in:" Browse, Find, Preview).

## Part Setup

[O, 2026-11-14, static: the serialized strings and frames of FileMaker Pro
26.0.3's compiled `IDD_PART_SETUP.nib` and `IDD_PART_DEFINITION.nib` in
`Contents/Resources/Base.lproj/`, read with `strings`; frames are AppKit's,
origin bottom left]

- **Part Setup** (window 444 x 243): Create `{330, 195}`, Change
  `{330, 163}`, Delete `{330, 131}` and Done `{330, 17}`, each 101 x 32;
  "List of parts", a one-column table 298 x 190 (its place is not in the
  strings).
- **Part Definition** (window 458 x 449): OK `{365, 13}` and Cancel
  `{284, 13}`, 79 x 32. Radio buttons at x 19, top to bottom: Top
  Navigation (y 411), Title Header (389), Header (367), Leading Grand
  Summary (345), Body (323), Sub-summary when sorted by (301), Trailing
  Grand Summary (279), Footer (257), Title Footer (235), Bottom Navigation
  (213). A pop-up `{273, 403}` 165 x 26 over "List of fields", a table
  `{275, 265}` 162 x 136. A separator at y 204. Checkboxes: Page break
  before each occurrence `{36, 182}`; Page break after every `{36, 160}`, a
  text field `{207, 157}` 36 x 22 and "occurrences" `{248, 161}`; Restart
  page numbers after each occurrence `{36, 138}`; Allow part to break
  across page boundaries `{36, 116}`; Discard remainder of part before new
  page `{53, 94}`, indented; Use alternate row state `{36, 70}` (with a
  color palette); Use active row state `{36, 50}`.

Which options each part type enables is in layout-model.md 10.15.

SHA-256 of the archives: `IDD_PART_SETUP.nib`
`3231311abf1fbf77a3d0511afc31b78796d82f84d7f3cf293579f6a32af195ec`;
`IDD_PART_DEFINITION.nib`
`a5068fd2b6fa7447f66a1950740445d0fdd9e16991de0710f588f64d17e68224`.

## Portal Setup

[O, 2026-11-14, static: the serialized strings and frames of FileMaker Pro
26.0.3's compiled `IDD_PORTAL_SETUP.nib` and `IDD_FIELD_ORDER.nib` in
`Contents/Resources/Base.lproj/`, read with `strings`; frames are AppKit's,
origin bottom left]

- **Portal Setup** (window 473 x 371): "Show records from:" `{20, 333}`
  and a pop-up `{145, 326}` 257 x 26. Checkboxes at x 107: Sort portal
  records (y 299) with Specify `{262, 290}` 98 x 32, Filter portal records
  (272) with Specify `{262, 263}`, Allow deletion of portal records
  (247), Allow vertical scrolling (223). "Show scroll bar:" `{125, 200}`
  and a pop-up `{229, 193}` 153 x 26 (Always, When scrolling); Reset
  scroll bar when exiting record `{124, 175}`. "Format" `{20, 144}` and a
  separator `{73, 147}` 376 wide. "Initial row:" `{101, 114}` and a text
  field `{180, 109}` 35 x 22; "Number of rows:" `{229, 114}` and a text
  field `{345, 109}` 35 x 22. Use alternate row state `{107, 84}` (with a
  color palette) and Use active row state `{107, 64}`. OK `{380, 13}` and
  Cancel `{301, 13}`, 79 x 32. Which bit each option sets is in
  layout-model.md 5.3.2.
- **Add Fields to Portal** (`IDD_FIELD_ORDER.nib`, window 507 x 286):
  "Available fields" `{20, 249}` over a pop-up `{18, 222}` 179 x 26 and a
  one-column table `{20, 55}` 175 x 164; "Included fields" `{312, 226}`
  over a table `{312, 55}` 175 x 164. Move `{203, 152}`, Move All
  `{203, 120}` and Clear All `{203, 88}`, 101 x 32. OK `{418, 8}` and
  Cancel `{337, 8}`, 79 x 32. [I] The window's title is set for the portal
  case; the nib is shared with other field-order dialogs.

[U] Not observed: which occurrences the pop-ups list, the default number
of rows for a drawn portal, how several fields are laid out, and what
Cancel in each dialog leaves.

SHA-256 of the archives: `IDD_PORTAL_SETUP.nib`
`79156cde832c546a5d557674e60e4fbd0e6d76de3fc12353501b505a13f9289b`;
`IDD_FIELD_ORDER.nib`
`9f9092a304f4ad8e17e95a18fab64058ed7717a7418c2524c87cbfe060cc6d89`.

## Inspector: a field's Data settings

[O, 2026-11-14, static: the serialized strings and bindings of FileMaker
Pro 26.0.3's compiled `FieldInspector.nib` and its six `FieldOptions*View.nib`
option views in `Contents/Resources/Base.lproj/`, read with `strings`; not
proof of runtime visibility or layout]

- **Field** (section title): "Display data from" with a field picker
  (`TableName::FieldName`, tooltip "Specify the field to display data
  from."); "Control style", a pop-up bound to `FieldControlStyle` by tag,
  enabled when the selection holds one field. Its items, in the archive's
  order: Edit Box, Drop-down List, Pop-up Menu, Checkbox Set, Radio Button
  Set, Drop-down Calendar, Concealed Edit Box (tooltip "Select control
  style."). Then "Placeholder text (when field is empty)" and "Show
  repetitions" from/to.
- **Options view per control** (shown under Control style):

  | Control | Labels, top to bottom as stored |
  |---|---|
  | Edit Box | "Show vertical scroll bar", "When scrolling", "Auto-complete using existing values" |
  | Drop-down List | "Values from:" pop-up and a button "Create or edit value lists.", "Allow editing of value list", "Include arrow to show and hide list", "Auto-complete using value list" |
  | Pop-up Menu | "Values from:" pop-up and button, "Allow editing of value list", "Allow entry of other values", "Include arrow to show and hide list", "Override data formatting with value list" |
  | Checkbox Set | "Values from:" pop-up and button, "Allow entry of other values", "Icon:" |
  | Radio Button Set | "Values from:" pop-up and button, "Allow entry of other values" |
  | Drop-down Calendar | "Include icon to show and hide calendar", "Auto-complete using existing values" |

- The "Values from:" pop-up lists the window's value lists by key and
  name (`DocWindowValueLists`, `FieldValueListKey`).

SHA-256 of the archives: `FieldInspector.nib`
`2fe529b8f350e3dad970d5cd393120a982b01dd96d8c90c5cbbac344c0edaf83`;
`FieldOptionsEditView.nib`
`7a5d26bbc8bc0e4e23bad8d738084d59ac28d97be581a0f6ba917e8355166147`;
`FieldOptionsDropDownView.nib`
`5880ddc0401105a7284e70f520cffd51b1878a0efc5a9521a7182f8853fd04c1`;
`FieldOptionsPopupView.nib`
`c2ae85ac0c49d41e68103a57bb4fba1209bb69bdafa8f6c7c9b02252b2180394`;
`FieldOptionsCheckboxView.nib`
`0a81f9810111be482223074cc8913f42dd2024b9af56320b419538b17b1413eb`;
`FieldOptionsRadioSetView.nib`
`860f66e7e295b32e02567303b4f153198f90af40cded9df9fcaf83fd68902ab2`;
`FieldOptionsCalendarView.nib`
`57a55590711b0c190bfca9f5f93e9eb27652dc476347b5f032e301e331065515`.

[O, 2026-11-14, FileMaker Pro 26.0.3, the walkthrough on [#71](https://github.com/OpenMaker-Dev/openmaker/issues/71)] The Data
tab read back a file's five changed controls as Control style and Values
from (Title's "Include arrow to show and hide list" checked); for an edit
box it shows no value list. Geometry and the pop-ups' widths are not
measured.

## Specify Calculation (capture 42)

Capture 42 shows FileMaker Pro 26's Specify Calculation for the Contacts
starter solution's `Initial` field, opened from Manage Database; the Claris
help page
[Specify Calculation](https://help.claris.com/en/pro-help/content/specify-calculation.html)
describes the rest. Measured from 42:

- **Window.** 1397 x 836 pt. Title bar 28 pt, the context row 30 pt, then
  the fields pane 223 pt, the editor, the operator strip 34 pt and the
  functions pane 242 pt, divided by 1 pt `#E6E6E6` rules. The bottom bar is
  78 pt with a calculation field's options ("Calculation result is",
  "Number of repetitions", "Do not evaluate if all referenced fields are
  empty", Storage Options).
- **Controls.** Labels and lists 11 pt (the system font), `L: 1 C: 1` 13 pt.
  Buttons, popups and search fields are macOS bezels: a light top edge, a
  darker `#B7B7B7` bottom edge and a soft shadow; the default button, popup
  steppers and checkboxes have a blue gradient `#3D4FD1` to `#1429C8`.
  Cancel and OK are 82 x 22 pt, 9 apart, 21 pt from the right and 19 from
  the bottom.
- **Context popup.** Disabled when the file has one occurrence.
- **Fields pane.** A table popup (`Current Table ("Contacts")`, then
  Related Tables and Unrelated Tables), a bars menu for the field order, a
  search field, and the fields with a type icon.
- **Functions pane.** Functions by name, case-insensitively, with the
  generic `Get ( flag )` listed as each documented selector
  (`Get ( ScriptParameter )`): the 28 rows visible in 42 match that order
  over `calc-functions.csv` and `get-selectors.csv`. A bars menu chooses
  the grouping (resource 135's category names); a search field and a
  Description panel with a "?" link follow.
- **Operator strip.** Four groups: `" "`, `( )`, `&`, `¶`; `=`, `≠`, `>`,
  `<`, `≥`, `≤`; `+`, `-`, `/`, `*`; `not`, `and`, `or`, `xor`, `^`.
- **Editor.** Monospaced 11 pt on 13 pt lines. Function names, Get
  selectors and the parentheses of their calls are teal `#00A8A9`, numbers
  magenta `#AB2079`; fields, variables, keywords (`not`), operators,
  separators and grouping parentheses are black. 42 has no text literal or
  comment to sample.

## Manage Database: the Fields tab's third column (Med: bin, 2026-11-13)

From FileMaker Pro 26 (Mac x86_64): `FMDefineFieldsPage`, `FMFieldListData` and
the app's string resource 329 (`0x149`), whose record array the app holds at
VA `0x1023ec050` (32-byte records of four string pointers, as DBEngine's,
`docs/re/catalogs`); indexes below are that resource's. Not compared with a
screen.

- **Header** `(329, 0x662)`: `Options / Comments / Advanced   (Click to
  toggle)`, three spaces before the parenthesis (`OnInitDialog`). The text
  does not change with the mode.
- **Modes.** Clicking the header steps the column through Options, Comments
  and Advanced and back (`FMFieldListData::Sort` with column 1000: mode + 1
  modulo 3); a new list starts on Options. Comments shows the field's
  comment (member `03`). Advanced shows "Display Names" `(329, 0x3da)` when
  bit `0x2` of the advanced flags is set, then the text of another member
  (`GetAdvancedFieldOptionsText`; members `29`-`31`, schema-path.md 5.5;
  which member is not traced).
- **Options** (`GetOptionsText`) is a list joined by ", " `(329, 0x3db)`
  (`AppendOption` adds it before every item but the first):
  1. "Global" `(0x965)` for option `0x10000`; "Unstored" `(0x3bf)` for
     `0x20000`; "Indexed" `(0x3be)` when any of the index bits
     `0xb8000000` is set (`DBField::GetIndexingChoice`).
  2. Then by kind.
- **Normal fields** (`GetNormalFieldOptionsText`), in this order
  (`v` = validation word):
  1. Container storage `0x1000`: DBEngine `(129, 1391)` "External (Secure)",
     or `(129, 1392)` "External (Open)" with `0x2000`.
  2. Furigana `0x40`: "Use Furigana" `(0x229)` with the furigana field
     (its exact text is not traced).
  3. `0x1`: the entry value, `(0x92e + byte 3)`: "Creation Date", "Creation
     Time", "Creation Timestamp (Date and Time)", "Creation Name", "Creation
     Account Name", then the five "Modification ..." texts.
  4. `0x2` or `0x200` "Auto-enter Serial"; `0x4` "Auto-enter Data"; `0x8`
     "Auto-enter Calculation", followed by " replaces existing value"
     `(0x3d6)` with `0x80`; `0x10` "Auto-enter Last Visited"; `0x20`
     "Evaluate Always"; `0x100` "Can't Modify Auto"; `0x400` "Lookup"
     (`0x3c1`-`0x3c8`).
  5. `v 0x400` "Always Validate"; `v 0x1` "By Calculation"; `v 0x100` "By
     Value List"; `v 0x8` "Required Value"; `v 0x10` "Unique"; `v 0x20`
     "Existing" (`0x3c9`-`0x3cf`).
  6. A strict type `v 0x7000`: `0x1000` "Numeric Only", `0x2000`
     "4-Digit Year Date", else "Time of Day" (`0x225`-`0x227`); `v 0x40`
     "Range"; `v 0x200` "Maximum".
  7. "Allow Override" `(0x3cc)` when `v 0x4` (strict) is clear and any of
     `v 0x7379` is set.
  8. "Message by Calculation" for `v 0x8000`, else "Message" for `v 0x80`.

  A starter's PrimaryKey (options `0x80000188`, validation `0x1c`) reads
  "Indexed, Auto-enter Calculation replaces existing value, Can't Modify
  Auto, Required Value, Unique" [I: derived].
- **Calculation fields** (`GetCalculatedFieldOptionsText`), after item 1:
  1. "from " `(0x3c7)` and the context occurrence's name when the field's
     table has two or more occurrences in the graph (`RebuildList` counts
     them), or "from <unknown>" (DBEngine `(129, 165)`) when the context is
     unknown.
  2. Then "= " and the formula text, each CR made a space and each LF
     dropped (`StripNonPrintingChars`); tabs stay.
- **Summary fields** (`GetSummaryFieldOptionsText`): "= " (not a list item),
  then the kind `(0x966 + kind)`: "Total of ", "Average of ", "Count of ",
  "Minimum of ", "Maximum of ", "Standard Deviation of ", "Fraction of Total
  of ", "List of "; then the summarized field's name. Japanese resources
  put the name first.
  - With the option flag `0x01`, or running `0x04` on any kind but List:
    Standard Deviation gets " (by population)" `(0x977)`; a List with
    `0x01` gets nothing; otherwise restart `0x10` gives " (running with
    restart), when sorted by " `(0x978)`, else `(0x96f + kind)`: Total and
    Count " (running)", Average ", weighted by ", Fraction ", when sorted
    by ".
  - Then the additional field's name for an Average or a Fraction, or a
    Total or Count with restart.

## FileMaker Pro 26.1.1

What 26.1.1 changes in the chrome and the developer dialogs (Claris release
notes, FileMaker Pro 26.1.1), with the binary evidence [O: FileMaker Pro
26.1.1's Mac app: `Base.lproj` nibs read with `strings`, Mac x86_64
symbols and strings]. Nothing here is measured on screen. Where the
options are stored is in the specs named.

- **macOS:** support for macOS 27 and its Liquid Glass look. New cell
  classes `LGButtonCell`, `LGComboBox`, `LGPopUpButtonCell`, `LGSlider`,
  `LGStepper`, `LGTextFieldCell`, the toolbar color
  `ToolbarHoverBackgroundColor26` and `FMModernToolbarCustomView` sizing.
  Claris's 26.1 launch webinar showed the toolbar in that style with the
  window title at the left [unmeasured].
- **Layout Setup**, General: "Show alerts when field isn't modifiable or
  no records are present", under "Show current record indicator in List
  View" (`IDD_LAYOUT_SETUP.nib`, SHA-256 `c3b0f9d5...`); on by default in new
  files, off in existing ones (Claris). Storage and the measured empty
  Form click are in layout-model.md 4.2.
- **Portal Setup:** "Place row for new record at top"
  (`IDD_PORTAL_SETUP.nib`, `8862dd0a...`); layout-model.md 5.3.2.
- **Inspector**, Data: "Show indicator for clipped contents" in the Edit
  Box options (`FieldOptionsEditView.nib`, `223f8cae...`, bound to
  `FieldClippedIndicator`) and "Display single checkbox without value" in
  the Checkbox Set options (`FieldOptionsCheckboxView.nib`, `6219939a...`,
  bound to `SingleUnlabeledCheckBox`); layout-model.md 5.4.1.
- **Button Setup, Button Bar Setup:** "Calculated icon" with a Specify
  button (`ButtonControllerMac SpecifyCalcIcon`, `hasCalcIcon`);
  layout-model.md 5.3.4.
- **Show Custom Dialog options:** "Close after (seconds):" (string
  resource 129); script-steps.md 12.4.
- **Edit > Paste in Place** (Shift-Command-V, Windows Ctrl+Shift+Alt+V;
  `ID_EDIT_PASTE_IN_PLACE`) pastes layout objects where they were copied
  from and makes the layout taller when needed.
- **Edit > Go to Line** (Command-L, Ctrl+L) in the Script Workspace and Edit
  Script (`GoToLinePanel.nib`, `SEGoToLineViewController`).
- **Specify Field** has four label placement buttons instead of "Create
  label", remembered per file (`CSpecifyFieldDialog::UpdatePlacementButtons`);
  where the choice is stored is [U].
- **Field lists** of Sort Records, Specify Field Order for Export and Modify
  Table View have a context menu, Show Field Display Names or Show Field
  Names (`Localizable.strings`).
- **Smaller changes** (Claris): Conditional Formatting's new conditions
  start as "Formula is"; Command-K also closes Open Quickly; Control and
  scrolling zooms the relationships graph around the pointer; the record
  count in the Browse toolbar widens past eight digits; the Inspector and
  Objects panes no longer animate; the privilege "Manage database, data
  sources, containers, and custom functions" is renamed "Manage database
  as a developer", and with it an account can use the Data Viewer without a
  running script and authenticate in the Script Debugger and Data Viewer;
  uploading to a host refuses a file name with a period, a slash or
  similar ("Filename contains an invalid character, such as a period or
  slash.", string resource 129); on macOS a web viewer asks before it
  downloads a file and marks the file with `com.apple.quarantine`.
- **Not announced: an agent interface.** The Mac app adds `ManageAgents.nib`
  ("Manage Agentic Access") and `AgentPrompt.nib`, and code for a local
  HTTP service on `127.0.0.1` with a configurable port that reports
  `{"service":"fmpro-dev","schema":1}`, lets a local program run scripts
  and Data API requests in open files (`performScript`, `ExecuteDataAPI`,
  `fmfetch`) after the user allows it with a Full Access account, and lists
  and revokes grants per program and file (`com.claris.fmp.agentgrant`).
  Whether it is enabled in the shipping build (`isAgentAccessAvailable`) is
  [U]; the file and host side are in session-identity.md 4.5 and
  network-protocol.md 5.2.

## Unknowns

- The nib decode covers only part of the serialized object graph:
  constraints, pixel layout, dynamic controls, runtime handlers and some
  hierarchy links. Runtime Layout mode behavior was not observed (the
  Computer Use pass could not change modes).
- What FileMaker Pro shows when its connection to the host is lost, and what
  it offers to reconnect; OpenMaker's workflow is its own
  ([recovery.md](../recovery.md)).
- FileMaker Pro's dialogs when a save meets a record another client changed,
  deleted or holds.
- Which record a window shows, and a script step acts on, after another
  client deletes the window's current record or a record before it in the
  found set. A record keeps its record id; that FileMaker Pro follows the
  id rather than the position is assumed, not observed
  ([recovery.md](../recovery.md), "Which record a command acts on").
- The Hosts dialog's wording and states for a host's file list; FileMaker
  Pro's GetFileList (op77) is not decoded (network-protocol.md 5.3).
- How a current layout hidden from the layout menus appears in the layout
  popup.
- Specify Calculation:
  - whether FileMaker qualifies unqualified field names of a step
    calculation on OK, and what the context popup opens on for a step
    (presumably the current layout's occurrence);
  - the alert texts: FileMaker may word calculation alerts differently from
    `error-codes.csv` (for example for 102 and 106), and may point at the
    error in other ways;
  - the colors of text literals, comments, variables and keywords; whether
    grouping parentheses are colored; the editor's tab stops;
  - the bars menus' exact items (resource 135 starts with `Name` and
    `Type`; the field orders are assumed), what a double click inserts over
    a selection and for Get functions, and whether operator buttons add
    spaces;
  - the bottom bar of a script step's dialog (42 is a field's, with the MBS
    plug-in's panel, which is not FileMaker's), the number field icon, and
    whether Japanese functions are listed in every locale;
  - how name completion while typing works (the help page: "start typing
    its name and pick from the list").
