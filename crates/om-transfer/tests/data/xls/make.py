"""Writes the `.xls` fixtures of `crates/om-transfer/tests/import_xls.rs`
with xlwt 1.3.0 (an independent BIFF8 writer); `python3 make.py <dir>`.

- `cells.xls`: sheet "Data" with a names row, text (a line feed, Latin-1,
  BMP and non-BMP characters), numbers written as RK and as NUMBER
  records (whole, decimal, negative, large), Booleans, a date, an empty
  gap and a second sheet "Other".
- `shared.xls`: 1,200 shared strings, enough for the SST to continue in
  CONTINUE records, most of them 16-bit, so the record boundaries fall
  inside strings of 16-bit characters.
"""
import datetime
import sys

import xlwt

out = sys.argv[1]

book = xlwt.Workbook(encoding="utf-8")
data = book.add_sheet("Data")
for c, name in enumerate(["Text", "Number", "Flag", "When"]):
    data.write(0, c, name)
rows = [
    ("plain", 7, True, datetime.date(2024, 2, 29)),
    ("two\nlines", 0.1, False, None),
    ("naïve – €100 😀 日本", -2.5, None, None),
    ("", 123456789012, None, None),
    (None, 1e21, None, None),
]
date = xlwt.easyxf(num_format_str="YYYY-MM-DD")
for r, (text, number, flag, when) in enumerate(rows, start=1):
    if text is not None:
        data.write(r, 0, text)
    data.write(r, 1, number)
    if flag is not None:
        data.write(r, 2, flag)
    if when is not None:
        data.write(r, 3, when, date)
data.write(7, 2, "after a gap")
other = book.add_sheet("Other")
other.write(0, 0, "second sheet")
book.save(f"{out}/cells.xls")

book = xlwt.Workbook(encoding="utf-8")
sheet = book.add_sheet("Strings")
for i in range(1200):
    text = f"string {i:04}" if i % 5 == 0 else f"строка {i:04} ✓"
    sheet.write(i, 0, text)
book.save(f"{out}/shared.xls")
