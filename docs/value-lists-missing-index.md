# Field values: value lists and Insert from Index

`NativeProvider` lists a field's values the same way for a value list that
uses a field's values and for Insert from Index: each distinct value of the
field's value index, in index order, shown by the lowest record that holds
it. It takes one of three paths. Each either returns the complete list or
fails with an error, never with part of the list.

A value-list control of a hosted file takes only the first path
(`DataProvider::host_field_values`): the host's list when it is the whole
list, else nothing, and the window then reads the field's records itself
(`docs/desktop-client.md`, "Value-list controls"). A local file's window
reads the records.

## The host's list

One QueryProgram: instruction 1 reads `[base table, 01, field, 10]` (the
value index's counts) with GetKey, and instruction 2 runs CreateValueListEx
for at most 100,000 values with instruction 2's access key
(`re/query-program.md` 8.5, 8.7). An existing 16-byte counts leaf means the
index exists, including one with no keys.

The host's list is the answer when its count equals the values it lists
and none of its keys is 219 bytes long. Otherwise it is incomplete:

- The host cut it at the request's maximum, with a negative count (8.7).
  Neither FileMaker Server nor OpenMaker Server cuts a list asked for
  without a prefix, as this one is (8.7, #465).
- A key is 219 bytes long: such a key may be cut (7.1), standing for
  several values that the host lists as one.

An incomplete list is never returned. The client reads the index instead.

## Reading the index

1. Up front, a field whose counts give more than 100,002 keys (100,000
   values, the empty value `01` and the long-key entry `ff 80`) is refused
   with error 3.
2. op90 downloads `[T, 01, field, 00]`, paged by the protocol
   (`re/network-protocol.md` 5.6.6c). The keys are read in order, leaving
   out `01` and `ff 80`. If more than 100,000 keys remain, error 3.
3. Each key's records come from its RID list. The client downloads the
   records it needs with op92, 500 at a time:
   - the lowest record of each key shorter than 219 bytes;
   - every record under a 219-byte key.
4. A record's candidates are its non-empty lines (a text field: the value
   index keys each line, 7.1) or its non-empty values (other types), in
   every declared repetition, repetition first, then line.
5. A key shorter than 219 bytes is one value: its lowest record's first
   candidate whose key, as the index holds it, is that key.
6. A 219-byte key is every distinct value among the candidates of all its
   records whose keys, cut to 219 bytes, are that key. Each distinct whole
   key is one value, shown by its lowest record's first such candidate, in
   whole-key order. Ordering by whole key keeps index order, since a cut
   key sorts where its values do.
7. The index and the records must agree: a key whose records hold no
   candidate with that key is an error, not a missing value.

## Without an index

When the counts are missing, the client scans every record of the table,
500 per batch (the index is never waited for), keys every candidate itself,
and lists each distinct whole key once, shown by its lowest record's first
such candidate, in key order. More than 100,000 distinct values is error 3,
not the first 100,000.

## What is complete and what is refused

Complete, on every path:

- every distinct value up to 100,000 values and 64 MiB of retained keys and
  text;
- long text values: two values that share a 219-byte cut key are two values
  when their whole keys differ;
- multi-line values, line by line, in every repetition (including
  repetition 256);
- the key forms of `re/query-program.md` 7.2-7.3: English and English
  international DUCET 5.2 primary weights, System and Unicode standard
  (the same, `00 00` and the secondary weights), Unicode raw (UTF-16BE),
  and the number, date, time and timestamp keys of 7.3 (negative numbers,
  fractions and fractions of a second included). Language 0 has no keys
  and lists nothing.

Refused with error 3:

- more than 100,000 values, or more than 64 MiB retained (instead of a short
  list);
- a value that has to be keyed and cannot be keyed exactly. Such values are:
  - text in any other index language (the tailored ones, 7.2);
  - text with no number in a typed field, which FileMaker keys as invalid
    (`fe ff`, 7.3);
  - numbers in a file whose locale (`[02, 0d]`) cannot be read: their text
    is read with the file's decimal and thousands separators;
  - dates outside the file's MDY, DMY or YMD order, or with no readable
    file locale (`[02, 0d]`, `re/file-metadata.md` 2).

  The scan keys every value. When reading the index, a value that cannot
  be keyed is still used where it is certain: as the value of a key whose
  lowest record holds no other candidate, and as the one value of a
  219-byte key whose records all hold that same text and nothing else.
  Fields of those forms with short lists keep the host's list.
- globals, unstored or client-computed calculations, summaries, containers,
  unreadable or nested cells and unknown types, when they must be read
  without an index.

## Evidence and choices

- `re/query-program.md` 7.1, 8.5 and 8.7 establish:
  - the index counts and GetKey's empty answer for a missing path;
  - instructions running in order;
  - CreateValueListEx's reply, its negative count for a list cut at the
    maximum, and the empty answer on an unindexed field;
  - the 219-byte key cut and the `ff 80` entry.

  Schema indexing flags alone are not enough (`re/schema-path.md` 5.5, 6,
  7.2).
- FileMaker Server lists every value of a request without a prefix, and
  past 1,000 bytes it writes the entries as a node of 1,000-byte pieces
  (8.7; observed live, #464). OpenMaker Server writes the same form
  (#465). This client joins the pieces, checking their order and the
  total, and `ff` is the number of values listed. A reply past `0xFFF00`
  bytes comes in pages (8.7, 11.1), which `Session::query` follows and
  joins; no paged reply of this request has been seen live.
- Dates use the file locale read explicitly from `[02, 0d]`
  (`re/file-metadata.md` 2), never an assumed MDY order. Stored text is
  used; layout formatting is not a parser (`re/data-format.md`).
- The reply is parsed strictly. Malformed counts, keys, key order or text,
  and unterminated replies, are errors.
- Showing a value by its lowest record is an inference (8.7, Med). Within
  one record, first repetition then first line is OpenMaker's deterministic
  choice, not an observed FileMaker rule.
- Telling values that share a cut key apart by their whole keys is
  OpenMaker's choice. FileMaker's own list shows one value per index key
  (8.7, inferred), so FileMaker Pro may list fewer values than OpenMaker
  for such fields.

## Boundaries

Custom lists and serial writes are unchanged. The first field's values over
all records remain the list for secondary-field and related-value options
(`re/file-metadata.md` 6). External and calculated lists remain
unsupported. Related-file lists, autocomplete and ExistsInIndex validation
are outside this feature. OpenMaker Server's own list shows a value by the
lowest record's primary cell, so for a repeating field it can differ from
what the client shows when it reads the index.

Tests use synthetic loopback files, literal native indexes and literal
native reply bytes. OpenMaker Server's late-index mode applies only to
finders, so a field with indexing prevented supplies the never-built
index's empty reply. No live FileMaker application or service is involved.
