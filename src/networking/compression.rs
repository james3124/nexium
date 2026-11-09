//! Content-Encoding decompression (gzip, deflate, brotli).

use std::io::Read;

use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};

use crate::utils::{NxError, Result};

pub fn decode_body(body: Bytes, content_encoding: Option<&str>, max_bytes: usize) -> Result<Bytes> {
    let Some(header) = content_encoding else {
        return Ok(body);
    };
    let encodings: Vec<String> = header
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|encoding| !encoding.is_empty() && encoding != "identity")
        .collect();
    let mut data = body;
    for encoding in encodings.iter().rev() {
        data = match encoding.as_str() {
            "gzip" | "x-gzip" => read_limited(GzDecoder::new(data.as_ref()), max_bytes)?,
            "br" => read_limited(brotli::Decompressor::new(data.as_ref(), 4096), max_bytes)?,
            "deflate" => decode_deflate(&data, max_bytes)?,
            other => {
                return Err(NxError::Network(format!(
                    "unsupported content-encoding: {other}"
                )));
            }
        };
    }
    Ok(data)
}

fn decode_deflate(data: &Bytes, max_bytes: usize) -> Result<Bytes> {
    match read_limited(ZlibDecoder::new(data.as_ref()), max_bytes) {
        Ok(decoded) => Ok(decoded),
        Err(_) => read_limited(DeflateDecoder::new(data.as_ref()), max_bytes),
    }
}

fn read_limited<R: Read>(reader: R, max_bytes: usize) -> Result<Bytes> {
    let mut out = Vec::new();
    let mut limited = reader.take(max_bytes as u64 + 1);
    limited
        .read_to_end(&mut out)
        .map_err(|error| NxError::Network(format!("decompression failed: {error}")))?;
    if out.len() > max_bytes {
        return Err(NxError::ResponseTooLarge(max_bytes));
    }
    Ok(Bytes::from(out))
}
