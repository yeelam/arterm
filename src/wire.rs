use anyhow::{bail, ensure, Context, Result};
use rmpv::Value;
use std::io::Cursor;

pub const MAX_FRAME: usize = 1024 * 1024;
pub fn s(value: &str) -> Value {
    Value::from(value)
}
pub fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}
pub fn get<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .as_map()
        .and_then(|m| m.iter().find(|(k, _)| k.as_str() == Some(key)))
        .map(|(_, v)| v)
        .with_context(|| format!("missing field {key}"))
}
pub fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    get(value, key)?
        .as_str()
        .with_context(|| format!("invalid string {key}"))
}
pub fn num(value: &Value, key: &str) -> Result<u64> {
    get(value, key)?
        .as_u64()
        .with_context(|| format!("invalid integer {key}"))
}
pub fn binary(value: &Value, key: &str) -> Result<Vec<u8>> {
    match get(value, key)? {
        Value::Binary(b) => Ok(b.clone()),
        _ => bail!("invalid binary {key}"),
    }
}
pub fn bin16(value: &Value, key: &str) -> Result<Vec<u8>> {
    let b = binary(value, key)?;
    ensure!(b.len() == 16, "invalid 128-bit {key}");
    Ok(b)
}
pub fn message(kind: &str, body: Value) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    map(vec![
        ("v", 1.into()),
        ("type", s(kind)),
        ("msg_id", id.into()),
        ("body", body),
    ])
}
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    rmpv::encode::write_value(&mut data, value)?;
    ensure!(data.len() <= MAX_FRAME, "frame too large");
    let mut result = (data.len() as u32).to_be_bytes().to_vec();
    result.extend(data);
    Ok(result)
}

#[derive(Default)]
pub struct Frames {
    buffer: Vec<u8>,
}
impl Frames {
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.buffer.len() + bytes.len() <= MAX_FRAME * 2 + 8,
            "frame buffer limit"
        );
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }
    pub fn next(&mut self) -> Result<Option<Value>> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        ensure!(len > 0 && len <= MAX_FRAME, "invalid frame length");
        if self.buffer.len() < len + 4 {
            return Ok(None);
        }
        let mut reader = Cursor::new(&self.buffer[4..4 + len]);
        let value = rmpv::decode::read_value_with_max_depth(&mut reader, 32)?;
        ensure!(reader.position() == len as u64, "trailing frame bytes");
        ensure!(num(&value, "v")? == 1, "unsupported protocol version");
        text(&value, "type")?;
        get(&value, "body")?;
        self.buffer.drain(..len + 4);
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn architecture_independent_wire_bytes() {
        let value = map(vec![
            ("v", 1.into()),
            ("type", s("Ping")),
            ("body", map(vec![("n", 0x0102030405060708u64.into())])),
        ]);
        let expected = b"\x00\x00\x00\x1f\x83\xa1v\x01\xa4type\xa4Ping\xa4body\x81\xa1n\xcf\x01\x02\x03\x04\x05\x06\x07\x08";
        assert_eq!(encode(&value).unwrap(), expected);
        let mut frames = Frames::default();
        frames.push(expected).unwrap();
        assert_eq!(frames.next().unwrap(), Some(value));
    }

    #[test]
    fn fragmented_and_coalesced() {
        let data = encode(&message("Ping", map(vec![]))).unwrap();
        let mut f = Frames::default();
        for byte in &data[..data.len() - 1] {
            f.push(&[*byte]).unwrap();
            assert!(f.next().unwrap().is_none());
        }
        f.push(&data[data.len() - 1..]).unwrap();
        f.push(&data).unwrap();
        assert_eq!(text(&f.next().unwrap().unwrap(), "type").unwrap(), "Ping");
        assert!(f.next().unwrap().is_some());
        assert!(f.next().unwrap().is_none());
    }
    #[test]
    fn rejects_oversized_frame() {
        let mut f = Frames::default();
        f.push(&u32::MAX.to_be_bytes()).unwrap();
        assert!(f.next().is_err());
    }
}
