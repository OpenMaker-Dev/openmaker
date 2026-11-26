# Layout data formats (`KFormat`)

Observed in the installed macOS arm64 FMEngine, 2026-10-29. Addresses below
are unslid virtual addresses, not file offsets. No customer records or binary
fixtures are required by the implementation tests. Claims cite the observed
reader, writer or formatter; **[I]** marks an inference.

`TLayoutBaseObjectAttrs::Read` (`0x2d7ce4`) reads the first two bytes of an
object's `KFormat` value as a text metric. `SaveExtendedAttributes`
(`0x48138`, value load at `0x4823c`) emits it as `fontHeight`.
The metric exists on ordinary two-byte label attributes too. It is a signed
16-bit value; a non-positive sentinel does not define a text height.

For objects with data formatting, the prefix is followed by the numeric,
date, and time streams. All multi-byte integers and UTF-16 units are big
endian. Strings below are fixed arrays terminated by a zero unit, with no
SCSU masking. The observed core is 210 bytes; field records append three
more bytes, the graphic format and one uninterpreted byte (below).

| Object-relative bytes | Member |
| --- | --- |
| 0–1 | `fontHeight` |
| 2–59 | numeric style, 58 bytes |
| 60–149 | date style, 90 bytes |
| 150–209 | time style, 60 bytes |
| 210–211 | graphic format (field records only) [I] |
| 212 | unknown, 0 in every observed record [U] |

## Numeric style

`TNumericStyle::Read` (`0x2d7e8c`) and `SaveNumFormat` (`0x4dd20`)
agree on the following fields. Offsets are relative to the numeric stream.

| Offset | Size | XML name / meaning |
| --- | --- | --- |
| 0 | 2 | `flags` |
| 2 | 1 | `charStyle` |
| 3 | 1 | `negativeStyle` |
| 4 | 12 | `currencySymbol`, six UTF-16 units; also custom units such as quotes |
| 16 | 2 | `thousandsSep` |
| 18 | 2 | `decimalPoint` |
| 20 | 4 | `negativeColor`, raw color bytes |
| 24 | 2 | `decimalDigits`, signed |
| 26 | 16 | `trueString`, eight UTF-16 units |
| 42 | 16 | `falseString`, eight UTF-16 units |

Flag behavior is established by `NumberFormatter::Format` (`0x2b395c`),
`DecimalFormat` (`0x2b7228`), and `ApplyNotation` (`0x2b7c48`):

| Mask | Behavior |
| --- | --- |
| `0x0001` | as entered; bypasses numeric formatting |
| `0x0002` | thousands separators |
| `0x0004` | show the symbol |
| `0x0008` | fixed decimal digits and decimal rounding |
| `0x0010` together with `0x0004` | multiply by 100 and append the localized percent symbol |
| `0x0020` | Boolean text; zero selects `falseString`, nonzero `trueString` |
| `0x0100` | general format |
| `0x0400` | apply the negative marker before applying the symbol; otherwise symbol first |
| `0x0800` | symbol before the number; otherwise append it |
| `0x1000` | negative-color indication |
| `0x2000` | blank a zero in decimal format |
| `0x4000` | system decimal/currency separators |

The starter solutions' currency fields set an unlisted `0x0200`, most with
`0x4000`: in Inventory's Product Details, three fields have `0x4a0c`
(symbol `$`, two decimals) and one `0x0a0c`. FileMaker Pro 26 on an en-US
system shows Unit Cost's `12.5` as `$12.50` [O: the alpha walkthrough,
[#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)]. Which separators, and which symbol, `0x4000` takes in another
locale is [U]; what `0x0200` does is [U].

Boolean and general dispatch precede decimal zero suppression. Percent
notation always appends its marker, independently of symbol-position flag.
`ApplyNegative` (`0x2b7998`) selects negative styles through resource strings;
styles 0 and 1 place the minus before and after the value respectively.
Other resource-based styles are not inferred by this implementation.

## Date style

Claris documents applying the Inspector's date and time formats to
`{{CurrentDate}}` and `{{CurrentTime}}`, like date and time fields
([Formatting fields](https://help.claris.com/archive/help/18/fmp/en/FMP_Help/formatting-fields.html)).
Browse sends these typed symbol results through the corresponding
supported format. The object's literals and other symbols retain their
normal presentation.

`TDateStyle::Read` (`0x2d8040`) and `SaveDateFormat` (`0x4e27c`):

| Offset | Size | XML name / meaning |
| --- | --- | --- |
| 0 | 1 | `format` |
| 1 | 1 | `charStyle` |
| 2 | 1 | `monthStyle` |
| 3 | 1 | `dayStyle` |
| 4 | 2 | `separator` |
| 6 | 4 | four `DateElement` byte codes |
| 10 | 80 | five `DateElementSep` arrays of eight UTF-16 units |

`DateFormatter::Format` (`0x2b365c`) bypasses custom rendering for format 0
(as entered). `CustomFormat` (`0x2b3b5c`) uses saved elements and all five
literal separators for format 1. Formats 2 and 7 call `GetNumericDate`
(`0x2b5b4c`): the file locale determines element order; format 2 uses a short
year and format 7 a full year. An unused saved element template therefore
does not change an as-entered date.

A FileMaker Pro 26.1.1 check on macOS in English (United States) observed
Long System Date format `09` at KFormat byte 60. It showed
`November 26, 2026` in date fields, merge fields and date symbols,
including symbols embedded in text. The `hhmm` time preset stored
`0083` and showed `4:31 AM` or `3:04 PM`, without seconds. All choices
survived reopen. The long-date output in other system locales is
unmeasured. [Native results](https://github.com/OpenMaker-Dev/openmaker/issues/1029)

The fixed named date choices store the same selector at KFormat byte 60
for a Date field and a Date Symbol. FileMaker Pro 26.1.1 preserved each
choice through reopen. The file and system both used English (United
States).

| Selector | Browse display |
| --- | --- |
| `03` | Dec 25, 2024 |
| `04` | December 25, 2024 |
| `05` | Wed, Dec 25, 2024 |
| `06` | Wednesday, December 25, 2024 |

Short System Date uses selector `08` and showed `12/25/24` in the same
run. Which locale supplies names for the fixed formats when file and
system locales differ is unmeasured.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1029#issuecomment-7191150217)

Date element codes follow `file-metadata.md` §2.3. `GetElement`
(`0x2b5d80`) formats numeric month/day with their own padding styles.
`AddLeadingChar` (`0x2b48d0`) proves style 0 means no padding, 1 zero
padding, and 2 space padding. A full year is padded to four digits; a short
year keeps its final two digits when the year has more than two digits.

## Time style

`TTimeStyle::Read` (`0x2d8184`) and `SaveTimeFormat` (`0x4e5f8`):

| Offset | Size | XML name / meaning |
| --- | --- | --- |
| 0 | 2 | `flags` |
| 2 | 1 | `charStyle` |
| 3 | 1 | `hourStyle` |
| 4 | 1 | `minsecStyle` |
| 5 | 1 | preserved reserved byte |
| 6 | 2 | `separator` |
| 8 | 16 | `amString`, eight UTF-16 units |
| 24 | 16 | `pmString`, eight UTF-16 units |
| 40 | 20 | `ampmString`, ten UTF-16 units, used by the 24-hour format |

`TimeFormatter::Format` (`0x2b34e0`), `CustomFormat` (`0x2b411c`),
`AppendTimeString` (`0x2b4ad0`) and `Get12Hour` (`0x2b5208`) establish:

| Mask | Behavior |
| --- | --- |
| `0x0001` | hours |
| `0x0002` | minutes |
| `0x0004` | seconds |
| `0x0008` | as entered |
| `0x0010` | 24-hour/elapsed clock; clear selects 12-hour conversion |
| `0x0020` | 24-hour marker before the time |
| `0x0040` | AM/PM marker before the time |
| `0x0080` | literal separator; clear uses localized hour/minute/second resources |
| `0x0600` | platform system-format paths |

Hour and minute/second padding use the same 0/1/2 styles as dates. Omitting
hours leaves elapsed minutes; omitting both hours and minutes leaves elapsed
seconds. Word separators, platform system formats, non-ASCII numeral styles,
and calendar-era formats remain explicit unsupported cases, retaining the
source display rather than substituting a different presentation.

## Timestamps

The stream has no timestamp style of its own: the observed record holds only
the numeric, date and time styles above. That a timestamp field's object
formats the date with its date style and the time of day with its time style
is inferred [I] from that layout; the timestamp path of the formatter has not
been traced.

OpenMaker's chosen behavior, not an observed rule: it joins the two parts with
one space, the separator of the locale's timestamp text (calc-values.md 4.5).
A value read as a date alone is midnight and one read as a time alone falls on
1/1/0001 (calc-values.md 4.6); a formatted part shows that date or time. A
part whose style is as entered keeps its text from the value, and is left out
when the value has no such text.

## A new field's record

[O, 2026-11-14, FileMaker Pro 26.0.3, the twins of #886 and the five
Pro-made blanks of layout-model.md 10.14] The 213-byte record Pro stores
for a field object or table view column it adds holds:

| Stream | Settings |
| --- | --- |
| text height | the theme's (14 on Apex Blue files Pro made, 10 on an OpenMaker-built one) |
| numeric | flags `0x0900` (General, symbol before), styles 0, symbol `$`, `,` and `.`, negative color `dd 00 00 00`, 2 digits, `Yes` and `No` |
| date | format 0 (as entered), styles 0, separator `/`, elements `03 06 01 08`, separators empty, `, `, ` `, `, `, empty |
| time | flags `0x008f` (as entered), hour style 0, minute and second style 1, reserved 0, separator `:`, ` AM`, ` PM`, no 24-hour marker |
| graphic, last byte | 5 (shrink, keep proportions), 0 |

The 156 field records of the English starter solutions differ only in what
General and as entered do not use (symbol `#`, 0 digits, an empty date
template, minute style 0), in a text height of 0, and in graphic format 7
on large containers. [I] Under General and as entered, the formatter uses
none of the symbol, separators, digits, strings or date template.

OpenMaker writes a new record from these settings, with the text height
0 that every starter field record carries, since it does not derive the
theme's (`DataFormat::new_field`). It writes a stored record again field
by field, and only when that gives it back byte for byte (`encode`). The
time style's reserved byte and the bytes from 210 on, which no field
explains, are kept as stored.

## Graphic format

How a field object fits a picture into its frame: the Inspector's image
format (crop, reduce, enlarge, reduce or enlarge, keep proportions,
alignment). Claris documents it as `graphicFormat` on a layout object's
`ExtendedAttributes` in the Database Design Report's XML, "bit flags
indicating the following properties" (FileMaker Pro 13, 18 and 19 DDR XML
grammars):

| Bit | Property |
| --- | --- |
| `0x0001` | shrink to fit |
| `0x0002` | grow to fit |
| `0x0004` | maintain proportions |
| `0x0008` | left align |
| `0x0010` | top align |
| `0x0020` | right align |
| `0x0080` | bottom align |
| `0x0040` | colorize (in the FileMaker Pro 19 grammar, not the 13) |
| `0x0100` | Optimize for: Interactive content (containers.md 6.6) [O] |
| `0x0200` | Start playback automatically, of an interactive container (containers.md 6.6) [O] |

[I] The `u16` at offset 210 of a field object's 213-byte record holds it.
The XML puts `graphicFormat` beside `fontHeight` in the same element, and
in the Contacts starter (en) each of the 23 field objects (Contact List 4,
Contact Details 19) has a 213-byte record: the 20 that show text store
`00 05 00` at 210–212, the large photo of Contact Details (object 41,
`Photo`, 325 x 215) `00 07 00`, its 80 x 54 header photo (object 50) and
Contact List's 80 x 44 row photo (object 12) `00 05 00`. Across the six
English starter solutions only these two values occur in the 156 field
object records: 7 on the seven large containers of the details layouts
(325 x 215 at 15, 116; three in Assets, one in each of the others but
Meetings), 5 on every other. 5 is shrink and maintain proportions; 7
adds grow. The reader of these bytes has not been traced, and no record
with an alignment or colorize bit has been seen.

[O] FileMaker Pro 26.0.3 placed two pictures in the container fields of
Assets, Contacts, Content Management, Inventory and Tasks, on all 15 of
their layouts that show one (#200). The pictures were a solid 400 x 200
and a 40 x 20 pixel PNG, inserted through Insert Picture. The captures
are at 2x, in a window of 1060 x 982 points:

| Object (frame, points) | Format | 400 x 200 shows at | 40 x 20 shows at |
| --- | --- | --- | --- |
| details photo, image, logo or file (325 x 215 stored, anchored to 361 wide) | 7 | 359 x 179.5, centered vertically | 359 x 179.5 |
| details header (80 x 54) | 5 | 78 x 39, centered | 40 x 20, centered |
| list row (80 x 44) | 5 | 78 x 39, centered | 40 x 20, centered |
| Tasks' attachment portal row (64 wide) | 5 | 62 x 31, centered | 40 x 20, centered |

- Each of these objects stores a border width of 1 point on every side
  and a border style of none (1), with no padding. The picture fits
  the frame less 1 point all round, so the width counts though no border
  is drawn, as with an edit box's text box (layout-model.md 6.5).
- That a wider stored border or padding insets the picture by its own width
  is [I].
- The table bears out format 7 (shrink, grow, proportions) and format 5
  (shrink and proportions: a smaller picture keeps its size). It also bears
  out centering on both axes without an alignment bit.

What the other settings do is inferred [I] from the Inspector's names, not
measured against FileMaker Pro:

- Neither shrink nor grow (crop): the picture keeps its size, one picture
  pixel to a point, and the frame clips it.
- Shrink: a picture larger than the frame is reduced to fit; a smaller one
  keeps its size. Grow: a smaller picture is enlarged to fit; a larger one
  keeps its size, clipped. Both: the picture always fits.
- With maintain proportions the limiting side fits and the other follows
  the picture's proportions; without it each side fits on its own (a
  picture made to fit fills the frame).
- No left or right bit centers the picture across the frame, no top or
  bottom bit centers it vertically; a picture larger than the frame is
  clipped on both sides. Both bits of one axis set: [U] (OpenMaker takes
  left or top).

`om_layout::GraphicFormat::place` implements this geometry.

**Merge fields.** A text or button object that holds a merge field stores
the same `KFormat` as a field object, and FileMaker shows the merged value
through it [O: the Assets starter's Asset Details header, object 35, a
button labelled `<<Price>>`, stores a `0d` byte for byte equal to the Price
field object's (object 16); FileMaker Pro 26.0.3 shows `$12.50` in both
(walkthrough [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201))]. Every number, date, time and timestamp merge field in
the object is formatted by that one format [I for objects holding several].

## Browse display in OpenMaker

Formatting changes only the text a field shows, and a merge field's value
in a text or button object (by the value's type), in Browse, Preview and
Save Records as PDF alike. An editor opens with the
value itself. The value is the field's text in the file's locale
(calc-values.md 2.7 and 4.5); the separators stored in the numeric style
present the number and never read it. With the system's separators
(`0x4000`), the file's own separators (its locale's) present it instead, and
the style's symbol stays [I: matches the en-US observation above; other
locales unmeasured]. A value that does not parse as the field's type keeps
its text.

These cases also keep the value's text rather than approximate a
presentation:

- numeric: general format (`0x0100`), negative styles other than 0 and 1, a
  negative `decimalDigits`, a nonzero `charStyle`. The negative color
  (`0x1000`) is not drawn.
- date: a format other than 0, 1, 2, 7 or 9, a nonzero `charStyle`, or
  element codes above 10. Format 9 uses the configured system locale for
  month-first English. Other system locales keep the original text.
- time: no hour, minute or second component, word separators, system formats,
  a nonzero `charStyle`, negative times, and fractional seconds when seconds
  are shown. The style stores no digit count for fractions.
