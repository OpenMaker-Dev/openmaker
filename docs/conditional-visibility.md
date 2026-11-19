# Conditional visibility in Browse

A layout object's "Hide object when" calculation (`KHideCondition`,
[layout-model.md](re/layout-model.md) 5.5) decides whether Browse paints the
object, its children, and whether it takes clicks. OpenMaker separates
FileMaker's own result from results it cannot produce:

| Visibility | When | Browse |
|---|---|---|
| Shown | the condition evaluated false | painted; a button runs its action |
| Hidden | the condition evaluated true, a `?` result included | nothing painted, children included; clicks reach what is underneath |
| Withheld | the data it reads, the calculation schema or the record is still being fetched | nothing painted and no clicks until the evaluation completes |
| Unresolved | OpenMaker cannot tell what FileMaker answers | a dashed, disabled stand-in at the object's place, without content, children or action |

Hidden follows FileMaker's truthiness of conditions (calc-values.md 2.9): a
`?` result is true. Errors the formula itself produces (`1/0`, a missing
field) are FileMaker results and hide the object. On one local FileMaker Pro
26.0.3 copy, Data Viewer evaluated `1/0` as `?` and the same formula hid a
text object in Browse [observed, [#38](https://github.com/OpenMaker-Dev/openmaker/issues/38)]. Other formulas returning `?`,
including a missing field, have not been compared in this condition.

Withheld and Unresolved are OpenMaker states, not FileMaker's. FileMaker Pro
evaluates synchronously and has every input. OpenMaker paints without waiting
for the provider. A condition is Unresolved when:

- its formula did not decode (`ObjectCalcs::undecodable` lists key 141);
- the window has no calculations for the layout: the provider gave no
  calculation schema, or the layout has no table occurrence;
- it read data the provider could not supply (a need the answer lacked); the
  `?` that reading shows elsewhere is not FileMaker's answer;
- it read a `Get` selector answered without the metadata FileMaker answers it
  from: the account's privilege set name, extended privileges, or layout and
  record access the client could not read (session-identity.md 2), in the
  root file or in a related file owning the formula;
- it called a function or plugin this client does not implement (the host's
  "not supported" fallback).

The stand-in takes the pointer but not the keyboard focus. It runs nothing,
and its click does not reach the canvas or the row underneath. Its
accessibility node is a disabled button for an object with an action, otherwise
a disabled label, named by the object's type and layout name. Nothing about the
record is shown. There is no "show everything" fallback for any account.

Objects without a hide condition are unaffected: they paint, with their
conditional formatting, tooltips and labels once those are calculated.
Those are calculated only while the object is in the window's view: one
outside it, scrolled or sized out of sight, waits until it is in view, as
FileMaker Pro evaluates them when it draws the object (#1120; inferred from
which related files it opened). An object with a hide condition is
evaluated in or out of view, since whether it shows also decides its
clicks and keyboard focus.

## Context

Hide conditions, conditional formatting, tooltips, placeholders and labels
evaluate on the record the object shows. In a portal they use the row; in a
List view they use each row's record. With an empty found set they evaluate on
no record, the way scripts do (script-runtime.md 13.21): fields read empty,
globals their values, and `Get` answers normally. While a record of a non-empty
found set is still being fetched, an object with a hide condition is withheld.

An object's `Self` is its content; a field object's is its field on that
record, in the object's own repetition. OpenMaker reads it once, before the
object's calculations run, and gives all of them that value, but only when
one of them may evaluate `Self`: it calls `Self` in any branch, a custom
function or `Evaluate`. Without such a formula the field's data is not read,
so a hidden summary is not computed; a total over a large found set behind
`Get ( FoundCount ) > 1000` reads nothing until the found set makes it visible
(#1023). Whether FileMaker Pro computes the content of an object it hides is
unmeasured ([layout-model.md](re/layout-model.md) 12); a hidden object shows
nothing either way.

The window's `Get` answers for layout calculations now include the current
view (`Get ( LayoutViewState )`). They also report a current record with an
unsaved draft as modified (`Get ( RecordOpenState )` 2), as a script in the
window reads them. Active field, modified field names and sort state remain
script-only in this client.

Cached results are keyed by layout, object, record and portal row. They expire
on data replies, writes, mode changes, variable changes from scripts, drafts and
targeted refreshes, as before. A result whose calculations read the window's
context (any `Get` selector or `GetLayoutObjectAttribute`) also expires when
that context changes. This covers another current record within the same page,
the window's name, view, geometry and focus, the found count, the layout and the
privilege sets. Results that read only data keep. Field contents keep their
existing refresh rules; an unstored field that reads `Get` is not re-evaluated
by a context change alone. When FileMaker re-evaluates a hide condition after
such a change is only partly measured. In local FileMaker Pro 26.0.3,
`Get ( FoundCount ) = 1` hid a text object immediately after a Find changed
the found set from three records to one, then showed it on Show All Records
[observed, [#38](https://github.com/OpenMaker-Dev/openmaker/issues/38)]. A script that set a global used by another hide condition
did not immediately update the visible object when launched from either
Script Workspace or the Scripts menu. Data Viewer confirmed the new value,
and the object was hidden after that later visit. In fresh local copies,
layout and mode changes hid the object, while clicks on the title or
empty layout header and changes to another tab panel left it visible
[observed, [#38](https://github.com/OpenMaker-Dev/openmaker/issues/38)]. The exact internal cache rule remains unmeasured. At
product code `ea6ca95d` (a commit of the project's earlier, private history), OpenMaker hid the object immediately after the
same script ran from its Scripts menu
on a byte-identical local copy. This observed case is a parity gap.

## Limits

- Native acceptance is partial. FileMaker Pro was compared on disposable
  local copies for one constant hide condition in Browse and Find ([#35](https://github.com/OpenMaker-Dev/openmaker/issues/35)),
  and for the expression and refresh cases above ([#38](https://github.com/OpenMaker-Dev/openmaker/issues/38)). Other
  expressions, object kinds and context changes remain unmeasured. The
  global-script case was compared against OpenMaker at `ea6ca95d` (of the
  earlier, private history);
  the other cases used an earlier debug binary of unknown revision.
- `Get ( UserName )` is Settings' User Name
  ([desktop-client.md](desktop-client.md#settings)): a condition on it
  matches FileMaker Pro set to the same name.
- Conditional formatting, tooltips and labels keep treating unreadable inputs
  as `?`. Only the hide condition has an Unresolved state.
- Find mode does not evaluate hide conditions, so an object whose
  condition FileMaker applies in Find mode (flag `0x100`,
  [layout-model.md](re/layout-model.md) 5.5.2) shows there. The constant
  `1` case was compared with FileMaker Pro [observed, [#35](https://github.com/OpenMaker-Dev/openmaker/issues/35)]. OpenMaker
  also evaluates a stored formula without checking flag `0x4`, which
  FileMaker requires; none of the scanned FileMaker-written files has a
  formula without that flag.
- A button bar's active segment calculation is evaluated with the bar's
  other calculations, on the same record, cache and context expiry
  ([scripting.md](scripting.md), "Desktop button bars"); in a portal row it
  is not evaluated and the bar says so. Buttons in portal rows take clicks
  under the same visibility rules ([scripting.md](scripting.md), "Desktop
  buttons in portal rows").
- Reports, Preview and PDF keep refusing objects with hide conditions
  ([portable-pdf.md](portable-pdf.md)).
- ExecuteSQL nested inside a hide condition does not report its own `Get`
  reads or unknown answers to the outer evaluation.
