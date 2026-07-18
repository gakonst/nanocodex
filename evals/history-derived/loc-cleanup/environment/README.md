# Parser cleanup

`parser_api.parse_records(lines)` is the only public API. `legacy_parser.py` is
obsolete and should be deleted. Consolidate on the smaller standards-compliant
implementation, preserving support for quoted CSV fields, whitespace around
unquoted values, blank records, and integer values.

The final production implementation must contain fewer nonblank lines than the
starting implementation. Do not retain a compatibility wrapper for the legacy
module.
