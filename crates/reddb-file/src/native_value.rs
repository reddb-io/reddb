//! Native entity-value bytes, relocated from the server without changing tags,
//! little-endian fields, varints, compression threshold or compression level.
use crate::embedded::{RdbFileError, RdbFileResult};
use crate::native_store::write_native_varu32 as write_varu32;
use reddb_types::Value;

fn invalid(error: impl std::fmt::Display) -> RdbFileError {
    RdbFileError::InvalidOperation(error.to_string())
}

/// Conservative accounting of allocations retained during one native decode.
/// Hosts reserve this allowance in their global scratch pool before decoding.
#[derive(Debug)]
pub struct DecodeBudget {
    pub limit_bytes: usize,
    pub used_bytes: usize,
}

impl DecodeBudget {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            used_bytes: 0,
        }
    }
    pub fn charge(&mut self, bytes: usize) -> RdbFileResult<()> {
        let required_bytes =
            self.used_bytes
                .checked_add(bytes)
                .ok_or(RdbFileError::ReadLimitExceeded {
                    required_bytes: usize::MAX,
                    limit_bytes: self.limit_bytes,
                })?;
        if required_bytes > self.limit_bytes {
            return Err(RdbFileError::ReadLimitExceeded {
                required_bytes,
                limit_bytes: self.limit_bytes,
            });
        }
        self.used_bytes = required_bytes;
        Ok(())
    }
    pub fn elements<T>(&mut self, count: usize) -> RdbFileResult<()> {
        let bytes =
            count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or(RdbFileError::ReadLimitExceeded {
                    required_bytes: usize::MAX,
                    limit_bytes: self.limit_bytes,
                })?;
        self.charge(bytes)
    }
    pub fn map<K, V>(&mut self, count: usize) -> RdbFileResult<()> {
        if count == 0 {
            return Ok(());
        }
        // Hash table capacity, controls, alignment and growth overlap. Admission
        // precedes allocation; count alone underestimates small/null values.
        let bytes = count
            .max(2)
            .checked_next_power_of_two()
            .and_then(|capacity| capacity.checked_mul(3))
            .and_then(|capacity| capacity.checked_mul(std::mem::size_of::<(K, V)>() + 1))
            .and_then(|bytes| bytes.checked_add(64))
            .ok_or(RdbFileError::ReadLimitExceeded {
                required_bytes: usize::MAX,
                limit_bytes: self.limit_bytes,
            })?;
        self.charge(bytes)
    }
}

/// Checked cursor shared by value decoding and the logical entity adapter.
pub struct NativeReader<'a, 'p> {
    data: &'a [u8],
    position: &'p mut usize,
    budget: Option<&'p mut DecodeBudget>,
}

impl<'a, 'p> NativeReader<'a, 'p> {
    pub fn new(data: &'a [u8], position: &'p mut usize) -> Self {
        Self {
            data,
            position,
            budget: None,
        }
    }
    pub fn with_budget(
        data: &'a [u8],
        position: &'p mut usize,
        budget: &'p mut DecodeBudget,
    ) -> Self {
        Self {
            data,
            position,
            budget: Some(budget),
        }
    }
    pub fn reserve_bytes(&mut self, bytes: usize) -> RdbFileResult<()> {
        match self.budget.as_mut() {
            Some(budget) => budget.charge(bytes),
            None => Ok(()),
        }
    }
    pub fn reserve_elements<T>(&mut self, count: usize) -> RdbFileResult<()> {
        match self.budget.as_mut() {
            Some(budget) => budget.elements::<T>(count),
            None => Ok(()),
        }
    }
    pub fn reserve_map<K, V>(&mut self, count: usize) -> RdbFileResult<()> {
        match self.budget.as_mut() {
            Some(budget) => budget.map::<K, V>(count),
            None => Ok(()),
        }
    }
    pub fn owned_bytes(&mut self) -> RdbFileResult<Vec<u8>> {
        let bytes = self.bytes()?;
        self.reserve_bytes(bytes.len())?;
        Ok(bytes.to_vec())
    }
    fn text(&mut self, compressed: bool) -> RdbFileResult<Value> {
        let bytes = if compressed {
            self.compressed()?
        } else {
            self.owned_bytes()?
        };
        // Text's shared owned representation may coexist with this Vec/String.
        self.reserve_bytes(bytes.len().saturating_add(2 * std::mem::size_of::<usize>()))?;
        Ok(Value::text(String::from_utf8(bytes).map_err(invalid)?))
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(*self.position)
    }
    pub fn take(&mut self, size: usize) -> RdbFileResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| invalid("native payload offset overflow"))?;
        let bytes = self
            .data
            .get(*self.position..end)
            .ok_or_else(|| invalid("truncated native payload"))?;
        *self.position = end;
        Ok(bytes)
    }
    pub fn fixed<const N: usize>(&mut self) -> RdbFileResult<[u8; N]> {
        Ok(self
            .take(N)?
            .try_into()
            .expect("checked native payload length"))
    }
    pub fn byte(&mut self) -> RdbFileResult<u8> {
        Ok(self.fixed::<1>()?[0])
    }
    pub fn variable_u32(&mut self) -> RdbFileResult<u32> {
        crate::native_store::read_native_varu32(
            self.data,
            self.position,
            "truncated native payload varu32",
        )
    }
    pub fn variable_u64(&mut self) -> RdbFileResult<u64> {
        crate::native_store::read_native_varu64(
            self.data,
            self.position,
            "truncated native payload varu64",
        )
    }
    pub fn length(&mut self) -> RdbFileResult<usize> {
        usize::try_from(self.variable_u32()?).map_err(invalid)
    }
    pub fn count(&mut self, minimum_bytes: usize) -> RdbFileResult<usize> {
        assert!(minimum_bytes > 0);
        let count = self.length()?;
        if count > self.remaining() / minimum_bytes {
            return Err(invalid("native payload count exceeds remaining bytes"));
        }
        Ok(count)
    }
    pub fn bytes(&mut self) -> RdbFileResult<&'a [u8]> {
        let size = self.length()?;
        self.take(size)
    }
    pub fn string(&mut self) -> RdbFileResult<String> {
        String::from_utf8(self.owned_bytes()?).map_err(invalid)
    }
    pub fn value(&mut self) -> RdbFileResult<Value> {
        read_native_value_from_reader(self)
    }
    fn compressed(&mut self) -> RdbFileResult<Vec<u8>> {
        let original = self.length()?;
        let compressed = self.bytes()?;
        let content_size = zstd::zstd_safe::get_frame_content_size(compressed)
            .map_err(|_| invalid("invalid native compressed value frame"))?;
        if let Some(size) = content_size {
            if size != original as u64 {
                return Err(invalid("native compressed value length mismatch"));
            }
        }
        self.reserve_bytes(original)?;
        let decoded = zstd::bulk::decompress(compressed, original).map_err(invalid)?;
        if decoded.len() != original {
            return Err(invalid("native compressed value length mismatch"));
        }
        Ok(decoded)
    }
}

/// Read one native value. Arrays use an explicit stack, retaining the same bytes
/// without consuming the host call stack on deeply nested persisted values.
pub fn read_native_value(data: &[u8], position: &mut usize) -> RdbFileResult<Value> {
    read_native_value_from_reader(&mut NativeReader::new(data, position))
}

fn read_native_value_from_reader(reader: &mut NativeReader<'_, '_>) -> RdbFileResult<Value> {
    let mut arrays: Vec<(usize, Vec<Value>)> = Vec::new();
    loop {
        let tag = reader.byte()?;
        if tag == 28 && arrays.len() >= reddb_types::utils::json::JSON_MAX_NESTING_DEPTH as usize {
            return Err(invalid("native value array nesting limit exceeded"));
        }
        let mut value = if tag == 28 {
            let count = reader.count(1)?;
            if count != 0 {
                reader.reserve_elements::<Value>(count)?;
                if arrays.len() == arrays.capacity() {
                    let capacity_next = arrays.capacity().max(2) * 2;
                    reader.reserve_elements::<(usize, Vec<Value>)>(capacity_next)?;
                    arrays.reserve_exact(capacity_next - arrays.len());
                }
                arrays.push((count, Vec::with_capacity(count)));
                continue;
            }
            Value::Array(Vec::new())
        } else {
            read_atom(reader, tag)?
        };
        loop {
            let Some((remaining, items)) = arrays.last_mut() else {
                return Ok(value);
            };
            items.push(value);
            *remaining -= 1;
            if *remaining != 0 {
                break;
            }
            value = Value::Array(arrays.pop().expect("completed native array frame").1);
        }
    }
}

fn read_atom(reader: &mut NativeReader<'_, '_>, tag: u8) -> RdbFileResult<Value> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    Ok(match tag {
        0 => Value::Null,
        1 => Value::Boolean(reader.byte()? != 0),
        2 => Value::Integer(i64::from_le_bytes(reader.fixed()?)),
        3 => Value::UnsignedInteger(u64::from_le_bytes(reader.fixed()?)),
        4 => Value::Float(f64::from_le_bytes(reader.fixed()?)),
        5 => reader.text(false)?,
        6 => Value::Blob(reader.owned_bytes()?),
        7 => Value::Timestamp(i64::from_le_bytes(reader.fixed()?)),
        8 => Value::Duration(i64::from_le_bytes(reader.fixed()?)),
        9 => {
            if reader.byte()? == 4 {
                Value::IpAddr(IpAddr::V4(Ipv4Addr::from(reader.fixed::<4>()?)))
            } else {
                Value::IpAddr(IpAddr::V6(Ipv6Addr::from(reader.fixed::<16>()?)))
            }
        }
        10 => Value::MacAddr(reader.fixed()?),
        11 => {
            let count = reader.count(4)?;
            reader.reserve_elements::<f32>(count)?;
            let mut vector = Vec::with_capacity(count);
            for _ in 0..count {
                vector.push(f32::from_le_bytes(reader.fixed()?));
            }
            Value::Vector(vector)
        }
        12 => Value::Json(reader.owned_bytes()?),
        13 => Value::Uuid(reader.fixed()?),
        14 => Value::NodeRef(reader.string()?),
        15 => Value::EdgeRef(reader.string()?),
        16 => Value::VectorRef(reader.string()?, u64::from_le_bytes(reader.fixed()?)),
        17 => Value::RowRef(reader.string()?, u64::from_le_bytes(reader.fixed()?)),
        18 => Value::Color(reader.fixed()?),
        19 => Value::Email(reader.string()?),
        20 => Value::Url(reader.string()?),
        21 => Value::Phone(u64::from_le_bytes(reader.fixed()?)),
        22 => Value::Semver(u32::from_le_bytes(reader.fixed()?)),
        23 => Value::Cidr(u32::from_le_bytes(reader.fixed()?), reader.byte()?),
        24 => Value::Date(i32::from_le_bytes(reader.fixed()?)),
        25 => Value::Time(u32::from_le_bytes(reader.fixed()?)),
        26 => Value::Decimal(i64::from_le_bytes(reader.fixed()?)),
        27 => Value::EnumValue(reader.byte()?),
        29 => Value::TimestampMs(i64::from_le_bytes(reader.fixed()?)),
        30 => Value::Ipv4(u32::from_le_bytes(reader.fixed()?)),
        31 => Value::Ipv6(reader.fixed()?),
        32 => Value::Subnet(
            u32::from_le_bytes(reader.fixed()?),
            u32::from_le_bytes(reader.fixed()?),
        ),
        33 => Value::Port(u16::from_le_bytes(reader.fixed()?)),
        34 => Value::Latitude(i32::from_le_bytes(reader.fixed()?)),
        35 => Value::Longitude(i32::from_le_bytes(reader.fixed()?)),
        36 => Value::GeoPoint(
            i32::from_le_bytes(reader.fixed()?),
            i32::from_le_bytes(reader.fixed()?),
        ),
        37 => Value::Country2(reader.fixed()?),
        38 => Value::Country3(reader.fixed()?),
        39 => Value::Lang2(reader.fixed()?),
        40 => Value::Lang5(reader.fixed()?),
        41 => Value::Currency(reader.fixed()?),
        42 => Value::ColorAlpha(reader.fixed()?),
        43 => Value::BigInt(i64::from_le_bytes(reader.fixed()?)),
        44 => Value::KeyRef(reader.string()?, reader.string()?),
        45 => Value::DocRef(reader.string()?, u64::from_le_bytes(reader.fixed()?)),
        46 => Value::TableRef(reader.string()?),
        47 => Value::PageRef(u32::from_le_bytes(reader.fixed()?)),
        48 => Value::Secret(reader.owned_bytes()?),
        49 => Value::Password(reader.string()?),
        50 => Value::AssetCode(reader.string()?),
        51 => Value::Money {
            asset_code: reader.string()?,
            scale: reader.byte()?,
            minor_units: i64::from_le_bytes(reader.fixed()?),
        },
        52 => Value::DecimalText(reader.string()?),
        53 => {
            let unit = reddb_types::TemperatureUnit::from_byte(reader.byte()?)
                .ok_or_else(|| invalid("invalid temperature unit"))?;
            let scale = reader.byte()?;
            let minor_units = i64::from_le_bytes(reader.fixed()?);
            Value::Temperature(
                reddb_types::Temperature::new(minor_units, scale, unit).map_err(invalid)?,
            )
        }
        54..=56 => {
            if reader.byte()? != 1 {
                return Err(invalid("unsupported native biometric envelope version"));
            }
            let format =
                reddb_types::BiometricFormat::from_code(u16::from_le_bytes(reader.fixed()?))
                    .ok_or_else(|| invalid("unknown native biometric format"))?;
            let modality = match tag {
                54 => reddb_types::BiometricModality::Fingerprint,
                55 => reddb_types::BiometricModality::Face,
                56 => reddb_types::BiometricModality::Iris,
                _ => unreachable!("match arm restricts native biometric tags"),
            };
            if format.modality() != modality {
                return Err(invalid("native biometric modality mismatch"));
            }
            let sample = reddb_types::BiometricSample::with_format(format, reader.owned_bytes()?)
                .map_err(invalid)?;
            match modality {
                reddb_types::BiometricModality::Fingerprint => Value::Fingerprint(sample),
                reddb_types::BiometricModality::Face => Value::Face(sample),
                reddb_types::BiometricModality::Iris => Value::Iris(sample),
            }
        }
        57 => {
            if reader.byte()? != 1 {
                return Err(invalid(
                    "unsupported native feature vector envelope version",
                ));
            }
            let space_id = reader.fixed()?;
            let dimensions = reader.count(std::mem::size_of::<f32>())?;
            reader.reserve_elements::<f32>(dimensions)?;
            let mut values = Vec::with_capacity(dimensions);
            for _ in 0..dimensions {
                values.push(f32::from_le_bytes(reader.fixed()?));
            }
            Value::FeatureVector(
                reddb_types::FeatureVector::new(space_id, values).map_err(invalid)?,
            )
        }
        58 => {
            if reader.byte()? != 1 {
                return Err(invalid("unsupported native bit signature envelope version"));
            }
            let space_id = reader.fixed()?;
            let length = reader.count(2)?;
            reader.reserve_bytes(
                length
                    .checked_mul(2)
                    .ok_or_else(|| invalid("native bit signature length overflow"))?,
            )?;
            let bits = reader.take(length)?.to_vec();
            let mask = reader.take(length)?.to_vec();
            Value::BitSignature(
                reddb_types::BitSignature::new(space_id, bits, Some(mask)).map_err(invalid)?,
            )
        }
        59 => {
            if reader.byte()? != 1 {
                return Err(invalid("unsupported native landmark envelope version"));
            }
            let space_id = reader.fixed()?;
            let dimensions = reader.byte()?;
            if !matches!(dimensions, 2 | 3) {
                return Err(invalid("invalid native landmark dimensions"));
            }
            let pair_count = reader.length()?;
            let midline_count = reader.length()?;
            let point_count = pair_count
                .checked_mul(2)
                .and_then(|count| count.checked_add(midline_count))
                .ok_or_else(|| invalid("native landmark point count overflow"))?;
            if pair_count < 2 || point_count > reddb_types::MAX_LANDMARK_POINTS {
                return Err(invalid("invalid native landmark point count"));
            }
            let coordinate_bytes = point_count
                .checked_mul(usize::from(dimensions))
                .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
                .ok_or_else(|| invalid("native landmark payload length overflow"))?;
            if coordinate_bytes > reader.remaining() {
                return Err(invalid("truncated native landmark payload"));
            }
            reader.reserve_elements::<reddb_types::LandmarkPair>(pair_count)?;
            reader.reserve_elements::<reddb_types::LandmarkCoordinates>(midline_count)?;
            let read_point = |reader: &mut NativeReader<'_, '_>| -> RdbFileResult<[f32; 3]> {
                let mut point = [0.0_f32; 3];
                for coordinate in point.iter_mut().take(usize::from(dimensions)) {
                    *coordinate = f32::from_le_bytes(reader.fixed()?);
                }
                Ok(point)
            };
            let mut pairs = Vec::with_capacity(pair_count);
            for _ in 0..pair_count {
                pairs.push((read_point(reader)?, read_point(reader)?));
            }
            let mut midline = Vec::with_capacity(midline_count);
            for _ in 0..midline_count {
                midline.push(read_point(reader)?);
            }
            Value::LandmarkSet(
                reddb_types::LandmarkSet::new(space_id, dimensions, pairs, midline)
                    .map_err(invalid)?,
            )
        }
        0x85 => reader.text(true)?,
        0x86 => Value::Blob(reader.compressed()?),
        _ => return Err(invalid(format!("Unknown Value type: {tag}"))),
    })
}

/// Append a value using the native writer's original byte layout and TOAST policy.
pub fn write_native_value(buf: &mut Vec<u8>, value: &Value) {
    use std::net::IpAddr;

    match value {
        Value::Null => buf.push(0),
        Value::Boolean(b) => {
            buf.push(1);
            buf.push(if *b { 1 } else { 0 });
        }
        Value::Integer(i) => {
            buf.push(2);
            buf.extend_from_slice(&i.to_le_bytes());
        }
        Value::UnsignedInteger(u) => {
            buf.push(3);
            buf.extend_from_slice(&u.to_le_bytes());
        }
        Value::Float(f) => {
            buf.push(4);
            buf.extend_from_slice(&f.to_le_bytes());
        }
        Value::Text(s) => {
            // C3 TOAST: compress Text values > 2 KiB with zstd.
            // Type 0x85 = compressed Text; type 5 = uncompressed (existing).
            // Layout for 0x85: [0x85][orig_len: varuint][comp_len: varuint][compressed bytes]
            const TEXT_COMPRESS_THRESHOLD: usize = 2048;
            if s.len() >= TEXT_COMPRESS_THRESHOLD {
                if let Ok(compressed) = zstd::bulk::compress(s.as_bytes(), 3) {
                    if compressed.len() < s.len() {
                        buf.push(0x85); // compressed Text marker
                        write_varu32(buf, s.len() as u32); // orig_len (decompression hint)
                        write_varu32(buf, compressed.len() as u32);
                        buf.extend_from_slice(&compressed);
                        return; // written — skip uncompressed path
                    }
                }
            }
            buf.push(5);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Blob(bytes) => {
            // C3 TOAST: compress Blob values > 2 KiB with zstd.
            // Type 0x86 = compressed Blob; type 6 = uncompressed (existing).
            const BLOB_COMPRESS_THRESHOLD: usize = 2048;
            if bytes.len() >= BLOB_COMPRESS_THRESHOLD {
                if let Ok(compressed) = zstd::bulk::compress(bytes.as_slice(), 3) {
                    if compressed.len() < bytes.len() {
                        buf.push(0x86); // compressed Blob marker
                        write_varu32(buf, bytes.len() as u32);
                        write_varu32(buf, compressed.len() as u32);
                        buf.extend_from_slice(&compressed);
                        return;
                    }
                }
            }
            buf.push(6);
            write_varu32(buf, bytes.len() as u32);
            buf.extend_from_slice(bytes);
        }
        Value::Timestamp(t) => {
            buf.push(7);
            buf.extend_from_slice(&t.to_le_bytes());
        }
        Value::Duration(d) => {
            buf.push(8);
            buf.extend_from_slice(&d.to_le_bytes());
        }
        Value::IpAddr(ip) => {
            buf.push(9);
            match ip {
                IpAddr::V4(v4) => {
                    buf.push(4);
                    buf.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    buf.push(6);
                    buf.extend_from_slice(&v6.octets());
                }
            }
        }
        Value::MacAddr(mac) => {
            buf.push(10);
            buf.extend_from_slice(mac);
        }
        Value::Vector(vec) => {
            buf.push(11);
            write_varu32(buf, vec.len() as u32);
            for f in vec {
                buf.extend_from_slice(&f.to_le_bytes());
            }
        }
        Value::Json(bytes) => {
            buf.push(12);
            write_varu32(buf, bytes.len() as u32);
            buf.extend_from_slice(bytes);
        }
        Value::Uuid(uuid) => {
            buf.push(13);
            buf.extend_from_slice(uuid);
        }
        Value::NodeRef(s) => {
            buf.push(14);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::EdgeRef(s) => {
            buf.push(15);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::VectorRef(s, id) => {
            buf.push(16);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
            buf.extend_from_slice(&id.to_le_bytes());
        }
        Value::RowRef(s, id) => {
            buf.push(17);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
            buf.extend_from_slice(&id.to_le_bytes());
        }
        Value::Color(rgb) => {
            buf.push(18);
            buf.extend_from_slice(rgb);
        }
        Value::Email(s) => {
            buf.push(19);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Url(s) => {
            buf.push(20);
            write_varu32(buf, s.len() as u32);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Phone(n) => {
            buf.push(21);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::Semver(v) => {
            buf.push(22);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Cidr(ip, prefix) => {
            buf.push(23);
            buf.extend_from_slice(&ip.to_le_bytes());
            buf.push(*prefix);
        }
        Value::Date(d) => {
            buf.push(24);
            buf.extend_from_slice(&d.to_le_bytes());
        }
        Value::Time(t) => {
            buf.push(25);
            buf.extend_from_slice(&t.to_le_bytes());
        }
        Value::Decimal(v) => {
            buf.push(26);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::EnumValue(i) => {
            buf.push(27);
            buf.push(*i);
        }
        Value::Array(elems) => {
            buf.push(28);
            write_varu32(buf, elems.len() as u32);
            for elem in elems {
                write_native_value(buf, elem);
            }
        }
        Value::TimestampMs(v) => {
            buf.push(29);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Ipv4(v) => {
            buf.push(30);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Ipv6(bytes) => {
            buf.push(31);
            buf.extend_from_slice(bytes);
        }
        Value::Subnet(ip, mask) => {
            buf.push(32);
            buf.extend_from_slice(&ip.to_le_bytes());
            buf.extend_from_slice(&mask.to_le_bytes());
        }
        Value::Port(v) => {
            buf.push(33);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Latitude(v) => {
            buf.push(34);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Longitude(v) => {
            buf.push(35);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::GeoPoint(lat, lon) => {
            buf.push(36);
            buf.extend_from_slice(&lat.to_le_bytes());
            buf.extend_from_slice(&lon.to_le_bytes());
        }
        Value::Country2(c) => {
            buf.push(37);
            buf.extend_from_slice(c);
        }
        Value::Country3(c) => {
            buf.push(38);
            buf.extend_from_slice(c);
        }
        Value::Lang2(c) => {
            buf.push(39);
            buf.extend_from_slice(c);
        }
        Value::Lang5(c) => {
            buf.push(40);
            buf.extend_from_slice(c);
        }
        Value::Currency(c) => {
            buf.push(41);
            buf.extend_from_slice(c);
        }
        Value::AssetCode(code) => {
            buf.push(50);
            write_varu32(buf, code.len() as u32);
            buf.extend_from_slice(code.as_bytes());
        }
        Value::Money {
            asset_code,
            minor_units,
            scale,
        } => {
            buf.push(51);
            write_varu32(buf, asset_code.len() as u32);
            buf.extend_from_slice(asset_code.as_bytes());
            buf.push(*scale);
            buf.extend_from_slice(&minor_units.to_le_bytes());
        }
        Value::ColorAlpha(rgba) => {
            buf.push(42);
            buf.extend_from_slice(rgba);
        }
        Value::BigInt(v) => {
            buf.push(43);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::KeyRef(col, key) => {
            buf.push(44);
            write_varu32(buf, col.len() as u32);
            buf.extend_from_slice(col.as_bytes());
            write_varu32(buf, key.len() as u32);
            buf.extend_from_slice(key.as_bytes());
        }
        Value::DocRef(col, id) => {
            buf.push(45);
            write_varu32(buf, col.len() as u32);
            buf.extend_from_slice(col.as_bytes());
            buf.extend_from_slice(&id.to_le_bytes());
        }
        Value::TableRef(name) => {
            buf.push(46);
            write_varu32(buf, name.len() as u32);
            buf.extend_from_slice(name.as_bytes());
        }
        Value::PageRef(page_id) => {
            buf.push(47);
            buf.extend_from_slice(&page_id.to_le_bytes());
        }
        Value::Secret(bytes) => {
            buf.push(48);
            write_varu32(buf, bytes.len() as u32);
            buf.extend_from_slice(bytes);
        }
        Value::Password(hash) => {
            buf.push(49);
            write_varu32(buf, hash.len() as u32);
            buf.extend_from_slice(hash.as_bytes());
        }
        Value::DecimalText(value) => {
            buf.push(52);
            write_varu32(buf, value.len() as u32);
            buf.extend_from_slice(value.as_bytes());
        }
        Value::Temperature(value) => {
            buf.push(53);
            buf.push(value.unit() as u8);
            buf.push(value.scale());
            buf.extend_from_slice(&value.minor_units().to_le_bytes());
        }
        Value::Fingerprint(value) => write_native_biometric(buf, 54, value),
        Value::Face(value) => write_native_biometric(buf, 55, value),
        Value::Iris(value) => write_native_biometric(buf, 56, value),
        Value::FeatureVector(value) => {
            buf.push(57);
            buf.push(1);
            buf.extend_from_slice(&value.space_id());
            write_varu32(
                buf,
                u32::try_from(value.dimensions()).expect("bounded feature vector dimensions"),
            );
            for component in value.values() {
                buf.extend_from_slice(&component.to_le_bytes());
            }
        }
        Value::BitSignature(value) => {
            buf.push(58);
            buf.push(1);
            buf.extend_from_slice(&value.space_id());
            write_varu32(
                buf,
                u32::try_from(value.len_bytes()).expect("bounded bit signature length"),
            );
            buf.extend_from_slice(value.bits());
            buf.extend_from_slice(value.mask());
        }
        Value::LandmarkSet(value) => {
            buf.push(59);
            buf.push(1);
            buf.extend_from_slice(&value.space_id());
            buf.push(value.dimensions());
            write_varu32(
                buf,
                u32::try_from(value.pairs().len()).expect("bounded landmark pair count"),
            );
            write_varu32(
                buf,
                u32::try_from(value.midline().len()).expect("bounded landmark midline count"),
            );
            let dimensions = usize::from(value.dimensions());
            for (left, right) in value.pairs() {
                for coordinate in left.iter().take(dimensions) {
                    buf.extend_from_slice(&coordinate.to_le_bytes());
                }
                for coordinate in right.iter().take(dimensions) {
                    buf.extend_from_slice(&coordinate.to_le_bytes());
                }
            }
            for point in value.midline() {
                for coordinate in point.iter().take(dimensions) {
                    buf.extend_from_slice(&coordinate.to_le_bytes());
                }
            }
        }
    }
}

fn write_native_biometric(buf: &mut Vec<u8>, tag: u8, value: &reddb_types::BiometricSample) {
    buf.push(tag);
    buf.push(1);
    buf.extend_from_slice(&(value.format() as u16).to_le_bytes());
    write_varu32(
        buf,
        u32::try_from(value.bytes().len()).expect("bounded biometric record length"),
    );
    buf.extend_from_slice(value.bytes());
}

/// Mandatory paged-file capabilities needed to decode this declared type.
pub fn required_file_capabilities_for_data_type(data_type: reddb_types::DataType) -> u64 {
    match data_type {
        reddb_types::DataType::Temperature => crate::FILE_CAPABILITY_TEMPERATURE,
        reddb_types::DataType::Fingerprint
        | reddb_types::DataType::Face
        | reddb_types::DataType::Iris => crate::FILE_CAPABILITY_BIOMETRIC_VALUES,
        reddb_types::DataType::FeatureVector | reddb_types::DataType::BitSignature => {
            crate::FILE_CAPABILITY_FEATURE_VALUES
        }
        reddb_types::DataType::LandmarkSet => crate::FILE_CAPABILITY_LANDMARK_VALUES,
        _ => 0,
    }
}

/// Mandatory paged-file capabilities needed to decode this value.
pub fn required_file_capabilities(value: &Value) -> u64 {
    match value {
        Value::Temperature(_) => {
            required_file_capabilities_for_data_type(reddb_types::DataType::Temperature)
        }
        Value::Fingerprint(_) | Value::Face(_) | Value::Iris(_) => {
            crate::FILE_CAPABILITY_BIOMETRIC_VALUES
        }
        Value::FeatureVector(_) | Value::BitSignature(_) => crate::FILE_CAPABILITY_FEATURE_VALUES,
        Value::LandmarkSet(_) => crate::FILE_CAPABILITY_LANDMARK_VALUES,
        Value::Array(values) => values.iter().fold(0, |required, value| {
            required | required_file_capabilities(value)
        }),
        _ => 0,
    }
}

/// Decode under an explicit allowance for expanded native values.
pub fn read_native_value_with_budget(
    data: &[u8],
    position: &mut usize,
    limit_bytes: usize,
) -> RdbFileResult<Value> {
    let mut budget = DecodeBudget::new(limit_bytes);
    budget.elements::<Value>(1)?;
    let mut next = *position;
    let value = read_native_value_from_reader(&mut NativeReader::with_budget(
        data,
        &mut next,
        &mut budget,
    ))?;
    *position = next;
    Ok(value)
}

#[cfg(test)]
mod landmark_tests {
    use super::*;

    fn landmark_value() -> Value {
        Value::LandmarkSet(
            reddb_types::LandmarkSet::new(
                [0x44; 16],
                2,
                vec![
                    ([-1.0, 1.0, 0.0], [1.0, 1.0, 0.0]),
                    ([-2.0, -1.0, 0.0], [2.0, -1.0, 0.0]),
                ],
                Vec::new(),
            )
            .expect("valid landmarks"),
        )
    }

    #[test]
    fn native_landmark_tag_and_capability_are_stable() {
        let value = landmark_value();
        let mut bytes = Vec::new();
        write_native_value(&mut bytes, &value);

        assert_eq!(bytes[0], 59);
        assert_eq!(bytes[1], 1);
        assert_eq!(crate::FILE_CAPABILITY_LANDMARK_VALUES, 1 << 8);
        assert_eq!(
            required_file_capabilities(&value),
            crate::FILE_CAPABILITY_LANDMARK_VALUES
        );
        assert_eq!(
            required_file_capabilities_for_data_type(reddb_types::DataType::LandmarkSet),
            crate::FILE_CAPABILITY_LANDMARK_VALUES
        );

        let mut position = 0;
        assert_eq!(read_native_value(&bytes, &mut position).unwrap(), value);
        assert_eq!(position, bytes.len());

        for end in 0..bytes.len() {
            let mut position = 0;
            assert!(
                read_native_value(&bytes[..end], &mut position).is_err(),
                "truncated native landmark decoded at byte {end}"
            );
        }
    }
}
