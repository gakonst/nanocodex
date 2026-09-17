//! Bounded-memory JSONL paging. Cursors are byte offsets into an immutable prefix.
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

const INLINE: u64 = 32 * 1024;
const CHUNK: usize = 32 * 1024;

pub(crate) struct Journal {
    file: tempfile::NamedTempFile,
    pub end: u64,
    pub error: Option<String>,
}
impl Journal {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            file: tempfile::NamedTempFile::new()?,
            end: 0,
            error: None,
        })
    }
    pub fn append(&mut self, value: &Value) {
        if self.error.is_some() {
            return;
        }
        let result = (|| -> io::Result<u64> {
            serde_json::to_writer(self.file.as_file_mut(), value)?;
            self.file.write_all(b"\n")?;
            self.file.as_file_mut().stream_position()
        })();
        match result {
            Ok(end) => self.end = end,
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn reader(&self) -> io::Result<File> {
        self.file.reopen()
    }
}

fn number(p: &Value, key: &str, default: u64) -> io::Result<u64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) => s.parse().map_err(io::Error::other),
        Some(v) => v.as_u64().ok_or_else(|| io::Error::other("invalid cursor")),
    }
}
fn previous_line(file: &mut File, end: u64) -> io::Result<u64> {
    if end == 0 {
        return Ok(0);
    }
    let mut pos = end - 1; // skip this record's trailing newline
    let mut buffer = [0; CHUNK];
    while pos > 0 {
        let start = pos.saturating_sub(CHUNK as u64);
        let len = (pos - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..len])?;
        if let Some(index) = buffer[..len].iter().rposition(|b| *b == b'\n') {
            return Ok(start + index as u64 + 1);
        }
        pos = start;
    }
    Ok(0)
}
fn next_line(file: &mut File, start: u64, boundary: u64) -> io::Result<u64> {
    file.seek(SeekFrom::Start(start))?;
    let mut pos = start;
    let mut buffer = [0; CHUNK];
    while pos < boundary {
        let len = (boundary - pos).min(CHUNK as u64) as usize;
        file.read_exact(&mut buffer[..len])?;
        if let Some(index) = buffer[..len].iter().position(|b| *b == b'\n') {
            return Ok(pos + index as u64 + 1);
        }
        pos += len as u64;
    }
    Err(io::Error::other("boundary splits a record"))
}
fn aligned(file: &mut File, cursor: u64) -> io::Result<()> {
    if cursor > 0 {
        file.seek(SeekFrom::Start(cursor - 1))?;
        let mut byte = [0];
        file.read_exact(&mut byte)?;
        if byte[0] != b'\n' {
            return Err(io::Error::other("cursor splits a record"));
        }
    }
    Ok(())
}

/// Newest-first by default. Pin `boundary` from the first page for subsequent reads.
pub fn page(mut file: File, committed: u64, p: &Value) -> io::Result<Value> {
    let boundary = number(p, "boundary", committed)?;
    if boundary > committed {
        return Err(io::Error::other("uncommitted boundary"));
    }
    aligned(&mut file, boundary)?;
    let forward = p["order"] == "oldest";
    let mut cursor = number(p, "cursor", if forward { 0 } else { boundary })?;
    if cursor > boundary {
        return Err(io::Error::other("cursor past boundary"));
    }
    aligned(&mut file, cursor)?;
    let limit = p["limit"].as_u64().unwrap_or(32).clamp(1, 16);
    let mut records = Vec::new();
    let mut bytes = 0;
    for _ in 0..limit {
        if (forward && cursor == boundary) || (!forward && cursor == 0) {
            break;
        }
        let (start, end) = if forward {
            (cursor, next_line(&mut file, cursor, boundary)?)
        } else {
            (previous_line(&mut file, cursor)?, cursor)
        };
        let length = end - start;
        let mut record = json!({"record_id":format!("{start}:{end}"),"start":start.to_string(),"end":end.to_string(),"bytes":length});
        if length <= INLINE {
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = vec![0; length as usize];
            file.read_exact(&mut bytes)?;
            record["value"] = serde_json::from_slice(&bytes)?;
        } else {
            record["chunked"] = json!(true);
        }
        bytes += record.to_string().len();
        if bytes > super::MAX_FRAME / 2 {
            break;
        }
        records.push(record);
        cursor = if forward { end } else { start };
    }
    Ok(
        json!({"boundary":boundary.to_string(),"records":records,"next_cursor":cursor.to_string(),
        "has_more": if forward { cursor < boundary } else { cursor > 0 }}),
    )
}

/// Byte arrays avoid UTF-8 splitting problems; concatenate then decode the original JSON.
pub fn chunk(mut file: File, committed: u64, p: &Value) -> io::Result<Value> {
    let boundary = number(p, "boundary", committed)?;
    let id = p["record_id"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing record_id"))?;
    let (start, end) = id
        .split_once(':')
        .ok_or_else(|| io::Error::other("invalid record_id"))?;
    let start: u64 = start.parse().map_err(io::Error::other)?;
    let end: u64 = end.parse().map_err(io::Error::other)?;
    if start >= end || end > boundary || boundary > committed {
        return Err(io::Error::other("invalid record range"));
    }
    let offset = number(p, "offset", 0)?;
    let length = end - start;
    if offset > length {
        return Err(io::Error::other("invalid chunk offset"));
    }
    let len = (length - offset).min(CHUNK as u64) as usize;
    file.seek(SeekFrom::Start(start + offset))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    Ok(
        json!({"record_id":id,"encoding":"utf8-bytes","bytes":bytes,"next_offset":(offset+len as u64).to_string(),
        "total_bytes":length.to_string(),"has_more":offset+(len as u64)<length}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_pages_pin_the_boundary_and_large_records_round_trip() {
        let mut journal = Journal::new().unwrap();
        journal.append(&json!({"first":1}));
        let large = json!({"text":"λ".repeat(600_000)});
        journal.append(&large);
        let boundary = journal.end;
        let page1 = page(journal.reader().unwrap(), boundary, &json!({"limit":1})).unwrap();
        assert_eq!(page1["records"][0]["chunked"], true);
        journal.append(&json!({"later":true}));
        let page2 = page(
            journal.reader().unwrap(),
            journal.end,
            &json!({"limit":1,"boundary":page1["boundary"],"cursor":page1["next_cursor"]}),
        )
        .unwrap();
        assert_eq!(page2["records"][0]["value"]["first"], 1);
        assert_eq!(page2["has_more"], false);
        let mut params =
            json!({"boundary":boundary.to_string(),"record_id":page1["records"][0]["record_id"]});
        let mut bytes = Vec::new();
        loop {
            let part = chunk(journal.reader().unwrap(), journal.end, &params).unwrap();
            bytes.extend(
                part["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8),
            );
            if part["has_more"] == false {
                break;
            }
            params["offset"] = part["next_offset"].clone();
        }
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), large);
        assert!(
            page(
                journal.reader().unwrap(),
                boundary,
                &json!({"boundary":boundary+1})
            )
            .is_err()
        );
        assert!(page(journal.reader().unwrap(), boundary, &json!({"cursor":2})).is_err());
    }
}
