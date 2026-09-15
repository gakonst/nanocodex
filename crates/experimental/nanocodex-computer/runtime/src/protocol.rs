use crate::{Error, Result};
use serde_json::{Value, json};
use std::io::{Read, Write};

/// The binary IPC prefix is a `u32`; this is the wire format's only size bound.
pub const MAX_FRAME: usize = u32::MAX as usize;
pub const IPC_VERSION: &str = "CodexComputerUseIPC-5";

#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        let mut messages = vec![];
        // Incremental storage even when the caller supplies many frames.
        for part in bytes.chunks(8192) {
            self.buffer.extend_from_slice(part);
            loop {
                if self.buffer.len() < 4 {
                    break;
                }
                let len = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
                let frame_len = len
                    .checked_add(4)
                    .ok_or_else(|| Error::invalid("Frame length exceeds address space"))?;
                if self.buffer.len() < frame_len {
                    break;
                }
                messages.push(serde_json::from_slice(&self.buffer[4..frame_len])?);
                self.buffer.drain(..frame_len);
            }
        }
        Ok(messages)
    }
    pub fn finish(&self) -> Result<()> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(Error::invalid("Truncated frame"))
        }
    }
}
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| Error::invalid("Frame exceeds the u32 wire length"))?;
    let mut result = len.to_le_bytes().to_vec();
    result.extend(bytes);
    Ok(result)
}
pub fn read_frame(input: &mut impl Read) -> Result<Option<Value>> {
    let mut header = [0; 4];
    match input.read(&mut header[..1])? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!(),
    };
    input.read_exact(&mut header[1..])?;
    let len = u32::from_le_bytes(header) as usize;
    let mut data = vec![0; len];
    input.read_exact(&mut data)?;
    Ok(Some(serde_json::from_slice(&data)?))
}
pub fn write_frame(output: &mut impl Write, value: &Value) -> Result<()> {
    output.write_all(&encode(value)?)?;
    output.flush()?;
    Ok(())
}
pub fn response(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
    }
}
pub fn validate_request(value: &Value) -> Result<()> {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || !value.get("method").is_some_and(Value::is_string)
    {
        return Err(Error::new(-32600, "Invalid JSON-RPC request"));
    }
    if let Some(id) = value.get("id")
        && !id.is_null()
        && !id.is_string()
        && !id.is_number()
    {
        return Err(Error::new(-32600, "Invalid request ID"));
    }
    if let Some(params) = value.get("params")
        && !params.is_object()
        && !params.is_array()
    {
        return Err(Error::new(-32600, "Invalid params"));
    }
    Ok(())
}
pub fn validate_response(value: &Value) -> Result<()> {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || !value
            .get("id")
            .is_some_and(|v| v.is_number() || v.is_string() || v.is_null())
        || value.get("result").is_some() == value.get("error").is_some()
    {
        return Err(Error::invalid("Invalid JSON-RPC response"));
    }
    if let Some(error) = value.get("error")
        && (!error["code"].is_i64() || !error["message"].is_string())
    {
        return Err(Error::invalid("Invalid JSON-RPC error"));
    }
    Ok(())
}
