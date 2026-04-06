// Binary serializer - compact and fast
use std::io::{self, Write};

#[derive(Debug, Clone)]
pub struct Record {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

pub struct Serializer;

impl Serializer {
    /// Serialize record to binary format
    /// Format: [total_len: u32][key_len: u32][key][value]
    pub fn serialize(record: &Record) -> io::Result<Vec<u8>> {
        let key_len = record.key.len() as u32;
        let value_len = record.value.len() as u32;
        let total_len = 4 + key_len + value_len; // 4 bytes for key_len

        let mut buf = Vec::with_capacity(4 + total_len as usize);

        // Total length (excluding this field)
        buf.write_all(&total_len.to_le_bytes())?;

        // Key length
        buf.write_all(&key_len.to_le_bytes())?;

        // Key
        buf.write_all(&record.key)?;

        // Value
        buf.write_all(&record.value)?;

        Ok(buf)
    }

    /// Deserialize record from binary format
    pub fn deserialize(buf: &[u8]) -> io::Result<Record> {
        if buf.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Buffer too small for key length",
            ));
        }

        let key_len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

        if buf.len() < 4 + key_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Buffer too small for key",
            ));
        }

        let key = buf[4..4 + key_len].to_vec();
        let value = buf[4 + key_len..].to_vec();

        Ok(Record { key, value })
    }

    /// Serialize a string efficiently
    pub fn serialize_string(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    /// Deserialize a string
    pub fn deserialize_string(buf: &[u8]) -> io::Result<String> {
        String::from_utf8(buf.to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Serialize u64 as little-endian bytes
    pub fn serialize_u64(n: u64) -> Vec<u8> {
        n.to_le_bytes().to_vec()
    }

    /// Deserialize u64 from little-endian bytes
    pub fn deserialize_u64(buf: &[u8]) -> io::Result<u64> {
        if buf.len() != 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid u64 buffer size",
            ));
        }
        Ok(u64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_deserialize() {
        let record = Record {
            key: b"domain:example.com".to_vec(),
            value: b"192.168.1.1".to_vec(),
        };

        let serialized = Serializer::serialize(&record).unwrap();
        let deserialized = Serializer::deserialize(&serialized[4..]).unwrap();

        assert_eq!(record.key, deserialized.key);
        assert_eq!(record.value, deserialized.value);
    }

    #[test]
    fn test_string_serialization() {
        let s = "example.com";
        let serialized = Serializer::serialize_string(s);
        let deserialized = Serializer::deserialize_string(&serialized).unwrap();
        assert_eq!(s, deserialized);
    }

    #[test]
    fn test_u64_serialization() {
        let n = 1234567890u64;
        let serialized = Serializer::serialize_u64(n);
        let deserialized = Serializer::deserialize_u64(&serialized).unwrap();
        assert_eq!(n, deserialized);
    }
}
