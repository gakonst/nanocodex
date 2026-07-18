import csv


def parse_records(lines):
    return [
        {"name": row[0].strip(), "value": int(row[1].strip())}
        for row in csv.reader(lines, skipinitialspace=True)
        if row
    ]
