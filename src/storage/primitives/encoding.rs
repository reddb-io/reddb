use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug)]
pub struct DecodeError(pub &'static str);

#[inline]
pub fn write_varu32(buf: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

#[inline]
pub fn write_varu64(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

#[inline]
pub fn write_vari32(buf: &mut Vec<u8>, value: i32) {
    let zigzag = ((value << 1) ^ (value >> 31)) as u32;
    write_varu32(buf, zigzag);
}

#[inline]
pub fn write_vari64(buf: &mut Vec<u8>, value: i64) {
    let zigzag = ((value << 1) ^ (value >> 63)) as u64;
    write_varu64(buf, zigzag);
}

#[inline]
pub fn read_varu32(bytes: &[u8], pos: &mut usize) -> Result<u32, DecodeError> {
    let mut result = 0u32;
    let mut shift = 0u32;
    while *pos < bytes.len() {
        let byte = bytes[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 35 {
            return Err(DecodeError("varu32 overflow"));
        }
    }
    Err(DecodeError("unexpected eof (varu32)"))
}

#[inline]
pub fn read_varu64(bytes: &[u8], pos: &mut usize) -> Result<u64, DecodeError> {
    let mut result = 0u64;
    let mut shift = 0u32;
    while *pos < bytes.len() {
        let byte = bytes[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 70 {
            return Err(DecodeError("varu64 overflow"));
        }
    }
    Err(DecodeError("unexpected eof (varu64)"))
}

#[inline]
pub fn read_vari32(bytes: &[u8], pos: &mut usize) -> Result<i32, DecodeError> {
    let raw = read_varu32(bytes, pos)?;
    Ok(((raw >> 1) as i32) ^ (-((raw & 1) as i32)))
}

#[inline]
pub fn read_vari64(bytes: &[u8], pos: &mut usize) -> Result<i64, DecodeError> {
    let raw = read_varu64(bytes, pos)?;
    Ok(((raw >> 1) as i64) ^ (-((raw & 1) as i64)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IpKey {
    pub bytes: [u8; 16],
    pub len: u8,
}

impl IpKey {
    pub fn from(addr: &IpAddr) -> Self {
        match addr {
            IpAddr::V4(v4) => {
                let mut bytes = [0u8; 16];
                bytes[..4].copy_from_slice(&v4.octets());
                Self { bytes, len: 4 }
            }
            IpAddr::V6(v6) => {
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&v6.octets());
                Self { bytes, len: 16 }
            }
        }
    }

    pub fn to_ip(self) -> IpAddr {
        if self.len == 4 {
            IpAddr::V4(Ipv4Addr::new(
                self.bytes[0],
                self.bytes[1],
                self.bytes[2],
                self.bytes[3],
            ))
        } else {
            IpAddr::V6(Ipv6Addr::from(self.bytes))
        }
    }
}

#[inline]
pub fn write_ip(buf: &mut Vec<u8>, addr: &IpAddr) {
    match addr {
        IpAddr::V4(v4) => {
            buf.push(0);
            buf.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            buf.push(1);
            buf.extend_from_slice(&v6.octets());
        }
    }
}

#[inline]
pub fn read_ip(bytes: &[u8], pos: &mut usize) -> Result<IpAddr, DecodeError> {
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (ip tag)"));
    }
    let tag = bytes[*pos];
    *pos += 1;
    match tag {
        0 => {
            if *pos + 4 > bytes.len() {
                return Err(DecodeError("unexpected eof (ipv4)"));
            }
            let mut octets = [0u8; 4];
            octets.copy_from_slice(&bytes[*pos..*pos + 4]);
            *pos += 4;
            Ok(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        1 => {
            if *pos + 16 > bytes.len() {
                return Err(DecodeError("unexpected eof (ipv6)"));
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&bytes[*pos..*pos + 16]);
            *pos += 16;
            Ok(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        _ => Err(DecodeError("invalid ip tag")),
    }
}

#[inline]
pub fn write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    write_varu32(buf, data.len() as u32);
    buf.extend_from_slice(data);
}

#[inline]
pub fn read_bytes<'a>(bytes: &'a [u8], pos: &mut usize) -> Result<&'a [u8], DecodeError> {
    let len = read_varu32(bytes, pos)? as usize;
    if *pos + len > bytes.len() {
        return Err(DecodeError("unexpected eof (bytes)"));
    }
    let slice = &bytes[*pos..*pos + len];
    *pos += len;
    Ok(slice)
}

#[inline]
pub fn write_string(buf: &mut Vec<u8>, value: &str) {
    write_bytes(buf, value.as_bytes());
}

#[inline]
pub fn read_string<'a>(bytes: &'a [u8], pos: &mut usize) -> Result<&'a str, DecodeError> {
    let data = read_bytes(bytes, pos)?;
    std::str::from_utf8(data).map_err(|_| DecodeError("invalid utf8"))
}

/// Write f64 as little-endian bytes
#[inline]
pub fn write_f64(buf: &mut Vec<u8>, value: f64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Read f64 from little-endian bytes
#[inline]
pub fn read_f64(bytes: &[u8], pos: &mut usize) -> Result<f64, DecodeError> {
    if *pos + 8 > bytes.len() {
        return Err(DecodeError("unexpected eof (f64)"));
    }
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&bytes[*pos..*pos + 8]);
    *pos += 8;
    Ok(f64::from_le_bytes(arr))
}

/// Write f32 as little-endian bytes
#[inline]
pub fn write_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Read f32 from little-endian bytes
#[inline]
pub fn read_f32(bytes: &[u8], pos: &mut usize) -> Result<f32, DecodeError> {
    if *pos + 4 > bytes.len() {
        return Err(DecodeError("unexpected eof (f32)"));
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(&bytes[*pos..*pos + 4]);
    *pos += 4;
    Ok(f32::from_le_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== VarInt Tests ====================

    #[test]
    fn test_varu32_single_byte() {
        let mut buf = Vec::new();
        write_varu32(&mut buf, 0);
        assert_eq!(buf, vec![0]);

        buf.clear();
        write_varu32(&mut buf, 1);
        assert_eq!(buf, vec![1]);

        buf.clear();
        write_varu32(&mut buf, 127);
        assert_eq!(buf, vec![127]);
    }

    #[test]
    fn test_varu32_multi_byte() {
        let mut buf = Vec::new();
        write_varu32(&mut buf, 128);
        assert_eq!(buf, vec![0x80, 0x01]);

        buf.clear();
        write_varu32(&mut buf, 300);
        let mut pos = 0;
        assert_eq!(read_varu32(&buf, &mut pos).unwrap(), 300);

        buf.clear();
        write_varu32(&mut buf, 16384);
        pos = 0;
        assert_eq!(read_varu32(&buf, &mut pos).unwrap(), 16384);
    }

    #[test]
    fn test_varu32_max() {
        let mut buf = Vec::new();
        write_varu32(&mut buf, u32::MAX);
        let mut pos = 0;
        assert_eq!(read_varu32(&buf, &mut pos).unwrap(), u32::MAX);
    }

    #[test]
    fn test_varu32_roundtrip() {
        let values = [
            0,
            1,
            127,
            128,
            255,
            256,
            16383,
            16384,
            2097151,
            2097152,
            u32::MAX,
        ];
        for &val in &values {
            let mut buf = Vec::new();
            write_varu32(&mut buf, val);
            let mut pos = 0;
            assert_eq!(
                read_varu32(&buf, &mut pos).unwrap(),
                val,
                "Failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_varu64_roundtrip() {
        let values = [0u64, 1, 127, 128, 255, 16384, u32::MAX as u64, u64::MAX];
        for &val in &values {
            let mut buf = Vec::new();
            write_varu64(&mut buf, val);
            let mut pos = 0;
            assert_eq!(
                read_varu64(&buf, &mut pos).unwrap(),
                val,
                "Failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_vari32_roundtrip() {
        let values = [0i32, 1, -1, 127, -128, i32::MAX, i32::MIN];
        for &val in &values {
            let mut buf = Vec::new();
            write_vari32(&mut buf, val);
            let mut pos = 0;
            assert_eq!(
                read_vari32(&buf, &mut pos).unwrap(),
                val,
                "Failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_vari64_roundtrip() {
        let values = [0i64, 1, -1, 127, -128, i64::MAX, i64::MIN];
        for &val in &values {
            let mut buf = Vec::new();
            write_vari64(&mut buf, val);
            let mut pos = 0;
            assert_eq!(
                read_vari64(&buf, &mut pos).unwrap(),
                val,
                "Failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_varu32_eof() {
        let buf = vec![0x80]; // Continuation bit set but no next byte
        let mut pos = 0;
        assert!(read_varu32(&buf, &mut pos).is_err());
    }

    #[test]
    fn test_varu32_overflow() {
        // More than 5 bytes with continuation bits
        let buf = vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x01];
        let mut pos = 0;
        assert!(read_varu32(&buf, &mut pos).is_err());
    }

    // ==================== IP Address Tests ====================

    #[test]
    fn test_ip_key_ipv4() {
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let key = IpKey::from(&addr);
        assert_eq!(key.len, 4);
        assert_eq!(key.bytes[..4], [192, 168, 1, 1]);
        assert_eq!(key.to_ip(), addr);
    }

    #[test]
    fn test_ip_key_ipv6() {
        let addr = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        let key = IpKey::from(&addr);
        assert_eq!(key.len, 16);
        assert_eq!(key.to_ip(), addr);
    }

    #[test]
    fn test_ip_key_ordering() {
        let ip1 = IpKey::from(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        let ip2 = IpKey::from(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        let ip3 = IpKey::from(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));

        assert!(ip1 < ip2);
        assert!(ip2 < ip3);
    }

    #[test]
    fn test_write_read_ip_v4() {
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let mut buf = Vec::new();
        write_ip(&mut buf, &addr);

        assert_eq!(buf.len(), 5); // 1 tag + 4 octets
        assert_eq!(buf[0], 0); // IPv4 tag

        let mut pos = 0;
        let decoded = read_ip(&buf, &mut pos).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn test_write_read_ip_v6() {
        let addr = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let mut buf = Vec::new();
        write_ip(&mut buf, &addr);

        assert_eq!(buf.len(), 17); // 1 tag + 16 octets
        assert_eq!(buf[0], 1); // IPv6 tag

        let mut pos = 0;
        let decoded = read_ip(&buf, &mut pos).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn test_read_ip_invalid_tag() {
        let buf = vec![2, 0, 0, 0, 0]; // Invalid tag
        let mut pos = 0;
        assert!(read_ip(&buf, &mut pos).is_err());
    }

    #[test]
    fn test_read_ip_truncated_v4() {
        let buf = vec![0, 192, 168]; // Only 2 octets
        let mut pos = 0;
        assert!(read_ip(&buf, &mut pos).is_err());
    }

    #[test]
    fn test_read_ip_truncated_v6() {
        let buf = vec![1, 0, 0, 0, 0, 0, 0, 0, 0]; // Only 8 octets
        let mut pos = 0;
        assert!(read_ip(&buf, &mut pos).is_err());
    }

    // ==================== Bytes/String Tests ====================

    #[test]
    fn test_write_read_bytes_empty() {
        let mut buf = Vec::new();
        write_bytes(&mut buf, &[]);

        let mut pos = 0;
        let decoded = read_bytes(&buf, &mut pos).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_write_read_bytes_small() {
        let data = b"hello";
        let mut buf = Vec::new();
        write_bytes(&mut buf, data);

        let mut pos = 0;
        let decoded = read_bytes(&buf, &mut pos).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_write_read_bytes_large() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let mut buf = Vec::new();
        write_bytes(&mut buf, &data);

        let mut pos = 0;
        let decoded = read_bytes(&buf, &mut pos).unwrap();
        assert_eq!(decoded, &data[..]);
    }

    #[test]
    fn test_write_read_string() {
        let s = "Hello, World! 🌍";
        let mut buf = Vec::new();
        write_string(&mut buf, s);

        let mut pos = 0;
        let decoded = read_string(&buf, &mut pos).unwrap();
        assert_eq!(decoded, s);
    }

    #[test]
    fn test_read_string_invalid_utf8() {
        let mut buf = Vec::new();
        write_varu32(&mut buf, 3);
        buf.extend_from_slice(&[0xFF, 0xFE, 0xFD]); // Invalid UTF-8

        let mut pos = 0;
        assert!(read_string(&buf, &mut pos).is_err());
    }

    #[test]
    fn test_read_bytes_truncated() {
        let mut buf = Vec::new();
        write_varu32(&mut buf, 100); // Says 100 bytes follow
        buf.extend_from_slice(&[1, 2, 3]); // Only 3 bytes

        let mut pos = 0;
        assert!(read_bytes(&buf, &mut pos).is_err());
    }

    // ==================== Multiple Values Tests ====================

    #[test]
    fn test_multiple_values_sequential() {
        let mut buf = Vec::new();

        // Write multiple values
        write_varu32(&mut buf, 42);
        write_ip(&mut buf, &IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        write_string(&mut buf, "test");
        write_vari32(&mut buf, -100);

        // Read them back
        let mut pos = 0;
        assert_eq!(read_varu32(&buf, &mut pos).unwrap(), 42);
        assert_eq!(
            read_ip(&buf, &mut pos).unwrap(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(read_string(&buf, &mut pos).unwrap(), "test");
        assert_eq!(read_vari32(&buf, &mut pos).unwrap(), -100);
        assert_eq!(pos, buf.len()); // Should have consumed everything
    }

    #[test]
    fn test_decode_error_display() {
        let err = DecodeError("test error");
        assert_eq!(err.0, "test error");
    }
}
