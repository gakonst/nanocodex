def _split(line):
    pieces = line.split(",")
    return [piece.strip() for piece in pieces]


def _convert(row):
    if len(row) != 2:
        raise ValueError("expected two columns")
    return {
        "name": row[0],
        "value": int(row[1]),
    }


def parse_records(lines):
    records = []
    for line in lines:
        if not line.strip():
            continue
        row = _split(line)
        record = _convert(row)
        records.append(record)
    return records
