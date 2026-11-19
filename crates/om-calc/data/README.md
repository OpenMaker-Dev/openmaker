# om-calc data

## `zh-han.bin`, `zh-stroke.bin`, `ko-hangul.bin`: Chinese and Korean ordering tables

FileMaker's order of Han characters in Chinese (Pinyin) files, under the
locale name Chinese_Stroke, and of Hangul jamo, Hanja and a set of
compatibility characters in Korean files
(`src/text/cjk.rs`, `docs/re/calc-values.md` 13, "Oracle answers
(collation gaps)").

- Source: FileMaker Server 26.0.3.309's calculation engine, run as a black
  box through `tools/fms-calc-harness` (battery `vc-cjk-order` in the
  Contacts templates `zh.fmp12` and `ko.fmp12`, `vc-cjk-stroke` with the
  locale name Chinese_Stroke in `en.fmp12`), 2026-10-29. The engine
  sorted every BMP character and one character per high surrogate of
  U+20000..U+2B7FF in ascending and descending input order; the tables are
  the resulting groups of characters that compare equal, numbered in
  order. No FileMaker code or data file was read.
- Generator: `python3 -I tools/fms-calc-harness/cjk_tables.py build
  <results> crates/om-calc/data` (format and anchors in its documentation).
- Sizes: `zh-han.bin` 56,014 bytes (27,508 characters, 27,057 ranks);
  `zh-stroke.bin` 56,014 bytes (27,967 characters, 27,516 ranks);
  `ko-hangul.bin` 58,624 bytes (28,967 characters, 24,275 ranks, 46 high
  surrogates).
- Public data compared (not used): CLDR's `zh` pinyin collations of
  releases 1.3 to 24 and Unihan 5.2.0 / 18.0.0 `kMandarin` and
  `kTotalStrokes` order the Hanzi differently (FileMaker sorts by pinyin,
  then stroke count, then an order of its own); CLDR's `zh` stroke
  collations (1.3 to 24) and `kTotalStrokes` with code point or
  radical-stroke order agree with Chinese_Stroke on about half of the
  neighbouring pairs only; CLDR's `ko` standard collation (1.4 to 21)
  lists 7,476 Hanja in FileMaker's order but FileMaker lists about 20,000
  more, many without a Korean reading in Unihan (`kHangul`, `kKorean`).
  The three tables are therefore observations, not derived from Unicode
  data; they carry no third-party license.
- The Japanese ordering needs no table: JIS X 0208 order comes from the
  `encoding_rs` EUC-JP encoder, the rest from code points
  (`src/text/cjk.rs`).
