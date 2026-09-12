## Numbers Computer Use

### Editing Spreadsheet Cells

To select a cell for editing, use a click. If the cell is empty or you’d like to append to it, use one click; to replace the existing contents of the cell, use three clicks.

When changing the value of the cell in a spreadsheet, return (in parallel): (1) a click tool call (with either one or three clicks) to select the cell and (2) a keyboard tool call to enter the new value(s).

Entering an entire row at a time (with \t delimiters) is great for batch entry, but do not try to enter multiple rows at a time, or multiple formulas a time, in one `type_text` call; it will fail.

Notes:
- Focused spreadsheet cells may include a ‘text entry area’ which includes additional formatting, such as Markdown bolding, in table headers; you can ignore this.
- Cell values are saved right away; there’s no need to press Return to confirm edits, unless you’re finished with the entire spreadsheet.
- To enter a value for a checkbox cell, you may type 0 or 1.
