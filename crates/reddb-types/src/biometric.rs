//! Bounded storage envelopes for standardized biometric records.

use std::fmt;

pub const MAX_BIOMETRIC_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_ANSI_NIST_RECORDS: usize = 65_536;
const GROUP_SEPARATOR: u8 = 0x1d;
const RECORD_SEPARATOR: u8 = 0x1e;
const UNIT_SEPARATOR: u8 = 0x1f;
const FILE_SEPARATOR: u8 = 0x1c;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BiometricModality {
    Fingerprint = 1,
    Face = 2,
    Iris = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum BiometricFormat {
    FingerprintIso39794_2 = 0x0101,
    FingerprintIso39794_4 = 0x0102,
    FingerprintIso19794_2 = 0x0103,
    FingerprintIso19794_4 = 0x0104,
    FingerprintAnsi378 = 0x0105,
    FingerprintAnsi381 = 0x0106,
    FingerprintAnsiNistType4 = 0x0107,
    FingerprintAnsiNistType9 = 0x0108,
    FingerprintCbeff = 0x0109,
    FaceIso39794_5 = 0x0201,
    FaceIso19794_5 = 0x0202,
    FaceAnsiNistType10 = 0x0203,
    FaceCbeff = 0x0204,
    IrisIso39794_6 = 0x0301,
    IrisIso19794_6 = 0x0302,
    IrisAnsiNistType17 = 0x0303,
    IrisCbeff = 0x0304,
}

impl BiometricFormat {
    pub fn modality(self) -> BiometricModality {
        match (self as u16) >> 8 {
            1 => BiometricModality::Fingerprint,
            2 => BiometricModality::Face,
            3 => BiometricModality::Iris,
            _ => unreachable!("all declared formats have a modality prefix"),
        }
    }

    pub fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            0x0101 => Self::FingerprintIso39794_2,
            0x0102 => Self::FingerprintIso39794_4,
            0x0103 => Self::FingerprintIso19794_2,
            0x0104 => Self::FingerprintIso19794_4,
            0x0105 => Self::FingerprintAnsi378,
            0x0106 => Self::FingerprintAnsi381,
            0x0107 => Self::FingerprintAnsiNistType4,
            0x0108 => Self::FingerprintAnsiNistType9,
            0x0109 => Self::FingerprintCbeff,
            0x0201 => Self::FaceIso39794_5,
            0x0202 => Self::FaceIso19794_5,
            0x0203 => Self::FaceAnsiNistType10,
            0x0204 => Self::FaceCbeff,
            0x0301 => Self::IrisIso39794_6,
            0x0302 => Self::IrisIso19794_6,
            0x0303 => Self::IrisAnsiNistType17,
            0x0304 => Self::IrisCbeff,
            _ => return None,
        })
    }

    pub fn standard_name(self) -> &'static str {
        match self {
            Self::FingerprintIso39794_2 => "ISO/IEC 39794-2",
            Self::FingerprintIso39794_4 => "ISO/IEC 39794-4",
            Self::FingerprintIso19794_2 => "ISO/IEC 19794-2",
            Self::FingerprintIso19794_4 => "ISO/IEC 19794-4",
            Self::FingerprintAnsi378 => "ANSI INCITS 378",
            Self::FingerprintAnsi381 => "ANSI INCITS 381",
            Self::FingerprintAnsiNistType4 => "ANSI/NIST-ITL Type-4",
            Self::FingerprintAnsiNistType9 => "ANSI/NIST-ITL Type-9",
            Self::FingerprintCbeff => "CBEFF fingerprint",
            Self::FaceIso39794_5 => "ISO/IEC 39794-5",
            Self::FaceIso19794_5 => "ISO/IEC 19794-5",
            Self::FaceAnsiNistType10 => "ANSI/NIST-ITL Type-10",
            Self::FaceCbeff => "CBEFF face",
            Self::IrisIso39794_6 => "ISO/IEC 39794-6",
            Self::IrisIso19794_6 => "ISO/IEC 19794-6",
            Self::IrisAnsiNistType17 => "ANSI/NIST-ITL Type-17",
            Self::IrisCbeff => "CBEFF iris",
        }
    }

    pub fn from_standard_name(name: &str) -> Option<Self> {
        const FORMATS: &[BiometricFormat] = &[
            BiometricFormat::FingerprintIso39794_2,
            BiometricFormat::FingerprintIso39794_4,
            BiometricFormat::FingerprintIso19794_2,
            BiometricFormat::FingerprintIso19794_4,
            BiometricFormat::FingerprintAnsi378,
            BiometricFormat::FingerprintAnsi381,
            BiometricFormat::FingerprintAnsiNistType4,
            BiometricFormat::FingerprintAnsiNistType9,
            BiometricFormat::FingerprintCbeff,
            BiometricFormat::FaceIso39794_5,
            BiometricFormat::FaceIso19794_5,
            BiometricFormat::FaceAnsiNistType10,
            BiometricFormat::FaceCbeff,
            BiometricFormat::IrisIso39794_6,
            BiometricFormat::IrisIso19794_6,
            BiometricFormat::IrisAnsiNistType17,
            BiometricFormat::IrisCbeff,
        ];
        FORMATS
            .iter()
            .copied()
            .find(|format| format.standard_name().eq_ignore_ascii_case(name))
    }

    fn permits_explicit_envelope_only(self) -> bool {
        matches!(
            self,
            Self::FingerprintIso39794_2
                | Self::FingerprintIso39794_4
                | Self::FaceIso39794_5
                | Self::IrisIso39794_6
                | Self::FingerprintAnsiNistType4
        )
    }
}

impl fmt::Display for BiometricFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.standard_name())
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BiometricSample {
    format: BiometricFormat,
    bytes: Vec<u8>,
}

impl fmt::Debug for BiometricSample {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BiometricSample")
            .field("modality", &self.modality())
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl BiometricSample {
    pub fn detect(modality: BiometricModality, bytes: Vec<u8>) -> Result<Self, BiometricError> {
        validate_size(&bytes)?;
        reject_standalone_image(&bytes)?;
        match detect_format(modality, &bytes) {
            Ok(format) => {
                validate_format(format, &bytes, false)?;
                Ok(Self { format, bytes })
            }
            Err(BiometricError::UnknownFormat | BiometricError::WrongModality) => {
                let (format, record) = extract_single_ansi_nist(modality, &bytes)?;
                validate_format(format, record, false)?;
                Ok(Self {
                    format,
                    bytes: record.to_vec(),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Constructs a sample whose format was carried by a typed wire, JSON, or
    /// CBEFF envelope. Explicit format does not bypass structural validation.
    pub fn with_format(format: BiometricFormat, bytes: Vec<u8>) -> Result<Self, BiometricError> {
        validate_size(&bytes)?;
        reject_standalone_image(&bytes)?;
        validate_format(format, &bytes, true)?;
        Ok(Self { format, bytes })
    }

    pub fn from_ansi_nist_record(record_type: u16, bytes: Vec<u8>) -> Result<Self, BiometricError> {
        let format = match record_type {
            4 => BiometricFormat::FingerprintAnsiNistType4,
            9 => BiometricFormat::FingerprintAnsiNistType9,
            10 => BiometricFormat::FaceAnsiNistType10,
            17 => BiometricFormat::IrisAnsiNistType17,
            99 => match cbeff_modality(&bytes)? {
                BiometricModality::Fingerprint => BiometricFormat::FingerprintCbeff,
                BiometricModality::Face => BiometricFormat::FaceCbeff,
                BiometricModality::Iris => BiometricFormat::IrisCbeff,
            },
            _ => return Err(BiometricError::UnsupportedRecordType),
        };
        Self::with_format(format, bytes)
    }

    pub fn format(&self) -> BiometricFormat {
        self.format
    }

    pub fn modality(&self) -> BiometricModality {
        self.format.modality()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Decode standardized minutiae into physical micrometre coordinates.
    /// Image-only fingerprint records intentionally return an error: RedDB
    /// never extracts minutiae from pixels.
    pub fn minutiae_views(&self) -> Result<Vec<MinutiaeView>, BiometricError> {
        match self.format {
            BiometricFormat::FingerprintIso19794_2 => {
                parse_fmr_views(&self.bytes, FmrLayout::Iso2005)
            }
            BiometricFormat::FingerprintAnsi378 if &self.bytes[4..8] == b"030\0" => {
                parse_fmr_views(&self.bytes, FmrLayout::Ansi2009)
            }
            BiometricFormat::FingerprintAnsi378 => {
                parse_fmr_views(&self.bytes, FmrLayout::Ansi2004)
            }
            _ => Err(BiometricError::MinutiaeTemplateRequired),
        }
    }

    /// Decode the 2D Landmark Point Block from an ISO/IEC 19794-5:2005
    /// record. Coordinates are copied from the structured record; no image
    /// processing or feature extraction is performed.
    pub fn face_landmarks(&self) -> Result<crate::LandmarkSet, crate::LandmarkError> {
        if self.format != BiometricFormat::FaceIso19794_5 {
            return Err(crate::LandmarkError::UnsupportedFaceFormat);
        }
        decode_fac_2005_landmarks(&self.bytes)
    }
}

fn decode_fac_2005_landmarks(bytes: &[u8]) -> Result<crate::LandmarkSet, crate::LandmarkError> {
    const GENERAL_HEADER_LENGTH: usize = 14;
    const FACE_INFORMATION_LENGTH: usize = 20;
    const FEATURE_POINT_LENGTH: usize = 8;
    // ISO/IEC 19794-5 feature-point type 1 uses the MPEG-4 A.B code
    // packed as `(A << 4) | B`. This deliberately small topology contains
    // only unambiguous bilateral points documented by the standard.
    const BILATERAL_CODES: &[(u8, u8)] = &[
        (0x37, 0x38), // inner eye corners
        (0x3b, 0x3c), // outer eye corners
        (0x91, 0x92), // paired nose points
        (0xc1, 0xc2), // eye centres
        (0xc3, 0xc4), // nostril centres
    ];
    const MIDLINE_CODES: &[u8] = &[0x9f];

    if bytes.len() < GENERAL_HEADER_LENGTH || bytes.get(12..14) != Some(&1_u16.to_be_bytes()) {
        return Err(crate::LandmarkError::AmbiguousFaceRecord);
    }
    let block_length = read_u32_be(bytes, GENERAL_HEADER_LENGTH)
        .map_err(|_| crate::LandmarkError::TruncatedLandmarkBlock)? as usize;
    let feature_count = read_u16_be(bytes, GENERAL_HEADER_LENGTH + 4)
        .map_err(|_| crate::LandmarkError::TruncatedLandmarkBlock)?
        as usize;
    let feature_bytes = feature_count
        .checked_mul(FEATURE_POINT_LENGTH)
        .ok_or(crate::LandmarkError::TruncatedLandmarkBlock)?;
    let points_start = GENERAL_HEADER_LENGTH
        .checked_add(FACE_INFORMATION_LENGTH)
        .ok_or(crate::LandmarkError::TruncatedLandmarkBlock)?;
    let points_end = points_start
        .checked_add(feature_bytes)
        .ok_or(crate::LandmarkError::TruncatedLandmarkBlock)?;
    let block_end = GENERAL_HEADER_LENGTH
        .checked_add(block_length)
        .ok_or(crate::LandmarkError::TruncatedLandmarkBlock)?;
    if points_end > block_end || block_end > bytes.len() {
        return Err(crate::LandmarkError::TruncatedLandmarkBlock);
    }

    let mut coordinates = [None; 256];
    for point in bytes[points_start..points_end]
        .as_chunks::<FEATURE_POINT_LENGTH>()
        .0
    {
        if point[0] != 1 {
            return Err(crate::LandmarkError::UnknownFeatureCode);
        }
        let code = point[1];
        let known = BILATERAL_CODES
            .iter()
            .any(|(left, right)| code == *left || code == *right)
            || MIDLINE_CODES.contains(&code);
        if !known {
            return Err(crate::LandmarkError::UnknownFeatureCode);
        }
        if coordinates[usize::from(code)].is_some() {
            return Err(crate::LandmarkError::DuplicateFeatureCode);
        }
        coordinates[usize::from(code)] = Some([
            f32::from(u16::from_be_bytes([point[2], point[3]])),
            f32::from(u16::from_be_bytes([point[4], point[5]])),
            0.0,
        ]);
    }

    let mut pairs = Vec::new();
    for (left_code, right_code) in BILATERAL_CODES {
        match (
            coordinates[usize::from(*left_code)],
            coordinates[usize::from(*right_code)],
        ) {
            (Some(left), Some(right)) => pairs.push((left, right)),
            (None, None) => {}
            _ => return Err(crate::LandmarkError::IncompleteTopology),
        }
    }
    if pairs.len() < 2 {
        return Err(crate::LandmarkError::IncompleteTopology);
    }
    let midline = MIDLINE_CODES
        .iter()
        .filter_map(|code| coordinates[usize::from(*code)])
        .collect();
    crate::LandmarkSet::new(crate::ISO_19794_5_2005_LANDMARK_SPACE_ID, 2, pairs, midline)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Minutia {
    pub x_micrometres: u32,
    pub y_micrometres: u32,
    /// Clockwise angle in millionths of a degree in `[0, 360_000_000)`.
    pub angle_microdegrees: u32,
    /// Standard minutia type: 0 other, 1 ridge ending, 2 bifurcation.
    pub kind: u8,
    pub quality: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MinutiaeView {
    pub position: u8,
    pub view: u8,
    pub minutiae: Vec<Minutia>,
}

fn validate_size(bytes: &[u8]) -> Result<(), BiometricError> {
    if bytes.is_empty() {
        return Err(BiometricError::EmptyRecord);
    }
    if bytes.len() > MAX_BIOMETRIC_RECORD_BYTES {
        return Err(BiometricError::RecordTooLarge {
            bytes: bytes.len(),
            maximum: MAX_BIOMETRIC_RECORD_BYTES,
        });
    }
    Ok(())
}

fn reject_standalone_image(bytes: &[u8]) -> Result<(), BiometricError> {
    let standalone = bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xFF, 0xA0]);
    if standalone {
        Err(BiometricError::StandaloneImage)
    } else {
        Ok(())
    }
}

fn detect_format(
    modality: BiometricModality,
    bytes: &[u8],
) -> Result<BiometricFormat, BiometricError> {
    match modality {
        BiometricModality::Fingerprint if bytes.starts_with(b"FMR\0") => detect_fmr(bytes),
        BiometricModality::Fingerprint if bytes.starts_with(b"FIR\0") => detect_fir(bytes),
        BiometricModality::Face if bytes.starts_with(b"FAC\0") => {
            Ok(BiometricFormat::FaceIso19794_5)
        }
        BiometricModality::Iris if bytes.starts_with(b"IIR\0") => {
            Ok(BiometricFormat::IrisIso19794_6)
        }
        _ => detect_ansi_nist(modality, bytes),
    }
}

fn detect_fmr(bytes: &[u8]) -> Result<BiometricFormat, BiometricError> {
    if bytes.len() < 12 {
        return Err(BiometricError::TruncatedRecord);
    }
    if &bytes[4..8] == b"030\0" {
        validate_fmr_layout(bytes, FmrLayout::Ansi2009)?;
        return Ok(BiometricFormat::FingerprintAnsi378);
    }
    if &bytes[4..8] != b" 20\0" {
        return Err(BiometricError::UnsupportedVersion);
    }
    let ansi = validate_fmr_layout(bytes, FmrLayout::Ansi2004).is_ok();
    let iso = validate_fmr_layout(bytes, FmrLayout::Iso2005).is_ok();
    match (ansi, iso) {
        (true, false) => Ok(BiometricFormat::FingerprintAnsi378),
        (false, true) => Ok(BiometricFormat::FingerprintIso19794_2),
        (true, true) => Err(BiometricError::AmbiguousFormat),
        (false, false) => Err(BiometricError::InvalidRecord),
    }
}

fn detect_fir(bytes: &[u8]) -> Result<BiometricFormat, BiometricError> {
    if bytes.len() < 8 {
        return Err(BiometricError::TruncatedRecord);
    }
    if &bytes[4..8] != b"010\0" {
        return Err(BiometricError::UnsupportedVersion);
    }
    let ansi = validate_fir(bytes, FirLayout::Ansi2004).is_ok();
    let iso = validate_fir(bytes, FirLayout::Iso2005).is_ok();
    match (ansi, iso) {
        (true, false) => Ok(BiometricFormat::FingerprintAnsi381),
        (false, true) => Ok(BiometricFormat::FingerprintIso19794_4),
        (true, true) => Err(BiometricError::AmbiguousFormat),
        (false, false) => Err(BiometricError::InvalidRecord),
    }
}

fn detect_ansi_nist(
    modality: BiometricModality,
    bytes: &[u8],
) -> Result<BiometricFormat, BiometricError> {
    let record_type = tagged_record_type(bytes).ok_or(BiometricError::UnknownFormat)?;
    let expected = match (modality, record_type) {
        (BiometricModality::Fingerprint, 9) => BiometricFormat::FingerprintAnsiNistType9,
        (BiometricModality::Face, 10) => BiometricFormat::FaceAnsiNistType10,
        (BiometricModality::Iris, 17) => BiometricFormat::IrisAnsiNistType17,
        (BiometricModality::Fingerprint, 99) => BiometricFormat::FingerprintCbeff,
        (BiometricModality::Face, 99) => BiometricFormat::FaceCbeff,
        (BiometricModality::Iris, 99) => BiometricFormat::IrisCbeff,
        _ => return Err(BiometricError::WrongModality),
    };
    if record_type == 99 {
        validate_cbeff_container(bytes, modality)?;
    } else {
        validate_tagged_record(bytes, record_type)?;
    }
    Ok(expected)
}

fn extract_single_ansi_nist(
    modality: BiometricModality,
    bytes: &[u8],
) -> Result<(BiometricFormat, &[u8]), BiometricError> {
    let record_type = match modality {
        BiometricModality::Fingerprint => 9,
        BiometricModality::Face => 10,
        BiometricModality::Iris => 17,
    };
    let mut found = None;
    for (kind, record) in ansi_nist_records(bytes)? {
        if kind != record_type {
            continue;
        }
        if found.is_some() {
            return Err(BiometricError::AmbiguousFormat);
        }
        found = Some(record);
    }
    let record = found.ok_or(BiometricError::WrongModality)?;
    let format = match modality {
        BiometricModality::Fingerprint => BiometricFormat::FingerprintAnsiNistType9,
        BiometricModality::Face => BiometricFormat::FaceAnsiNistType10,
        BiometricModality::Iris => BiometricFormat::IrisAnsiNistType17,
    };
    Ok((format, record))
}

/// Select one logical record from an ANSI/NIST-ITL transaction. Occurrences
/// are one-based. Binary records are located through the Type-1 CNT directory.
pub fn select_ansi_nist_record(
    bytes: &[u8],
    record_type: u16,
    occurrence: usize,
) -> Result<Vec<u8>, BiometricError> {
    if occurrence == 0 {
        return Err(BiometricError::InvalidOccurrence);
    }
    ansi_nist_records(bytes)?
        .into_iter()
        .filter(|(kind, _)| *kind == record_type)
        .nth(occurrence - 1)
        .map(|(_, record)| record.to_vec())
        .ok_or(BiometricError::RecordNotFound)
}

pub fn select_ansi_nist_sample(
    bytes: &[u8],
    record_type: u16,
    occurrence: usize,
) -> Result<BiometricSample, BiometricError> {
    let record = select_ansi_nist_record(bytes, record_type, occurrence)?;
    BiometricSample::from_ansi_nist_record(record_type, record)
}

fn ansi_nist_records(bytes: &[u8]) -> Result<Vec<(u16, &[u8])>, BiometricError> {
    if tagged_record_type(bytes) != Some(1) {
        return tagged_records(bytes);
    }

    let type_one_length = tagged_declared_length(bytes, 1)?;
    let type_one = bytes
        .get(..type_one_length)
        .ok_or(BiometricError::TruncatedRecord)?;
    validate_tagged_record(type_one, 1)?;
    let record_types = parse_type_one_cnt(type_one)?;
    let mut records = Vec::with_capacity(record_types.len().saturating_add(1));
    records.push((1, type_one));
    let mut offset = type_one_length;
    for record_type in record_types {
        let remaining = bytes.get(offset..).ok_or(BiometricError::TruncatedRecord)?;
        let tagged = tagged_record_type(remaining) == Some(record_type);
        let declared = if tagged {
            tagged_declared_length(remaining, record_type)?
        } else {
            read_u32_be(remaining, 0)? as usize
        };
        if declared == 0 {
            return Err(BiometricError::InvalidRecord);
        }
        let end = offset
            .checked_add(declared)
            .ok_or(BiometricError::InvalidRecord)?;
        let record = bytes
            .get(offset..end)
            .ok_or(BiometricError::TruncatedRecord)?;
        if tagged {
            validate_tagged_record(record, record_type)?;
        } else if record_type == 4 {
            validate_ansi_nist_type_four(record)?;
        }
        records.push((record_type, record));
        offset = end;
    }
    if offset != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(records)
}

fn parse_type_one_cnt(type_one: &[u8]) -> Result<Vec<u16>, BiometricError> {
    let cnt = type_one
        .split(|byte| *byte == GROUP_SEPARATOR || *byte == FILE_SEPARATOR)
        .find_map(|field| field.strip_prefix(b"1.003:"))
        .ok_or(BiometricError::InvalidRecord)?;
    let mut subfields = cnt.split(|byte| *byte == RECORD_SEPARATOR);
    let first = subfields.next().ok_or(BiometricError::InvalidRecord)?;
    let mut first_items = first.split(|byte| *byte == UNIT_SEPARATOR);
    if parse_ascii_u16(first_items.next().ok_or(BiometricError::InvalidRecord)?)? != 1 {
        return Err(BiometricError::InvalidRecord);
    }
    let declared_count =
        parse_ascii_usize(first_items.next().ok_or(BiometricError::InvalidRecord)?)?;
    if first_items.next().is_some() || declared_count > MAX_ANSI_NIST_RECORDS {
        return Err(BiometricError::InvalidRecord);
    }
    let mut record_types = Vec::with_capacity(declared_count);
    for subfield in subfields {
        let mut items = subfield.split(|byte| *byte == UNIT_SEPARATOR);
        let record_type = parse_ascii_u16(items.next().ok_or(BiometricError::InvalidRecord)?)?;
        let idc = items.next().ok_or(BiometricError::InvalidRecord)?;
        if idc.is_empty() || !idc.iter().all(u8::is_ascii_digit) || items.next().is_some() {
            return Err(BiometricError::InvalidRecord);
        }
        record_types.push(record_type);
    }
    if record_types.len() != declared_count {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(record_types)
}

fn parse_ascii_u16(bytes: &[u8]) -> Result<u16, BiometricError> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(BiometricError::InvalidRecord)
}

fn parse_ascii_usize(bytes: &[u8]) -> Result<usize, BiometricError> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(BiometricError::InvalidRecord)
}

fn tagged_records(bytes: &[u8]) -> Result<Vec<(u16, &[u8])>, BiometricError> {
    let mut records = Vec::new();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        let record_type = tagged_record_type(remaining).ok_or(BiometricError::UnknownFormat)?;
        let declared = tagged_declared_length(remaining, record_type)?;
        if declared == 0 {
            return Err(BiometricError::InvalidRecord);
        }
        let end = offset
            .checked_add(declared)
            .ok_or(BiometricError::InvalidRecord)?;
        let record = bytes
            .get(offset..end)
            .ok_or(BiometricError::TruncatedRecord)?;
        validate_tagged_record(record, record_type)?;
        records.push((record_type, record));
        offset = end;
    }
    Ok(records)
}

fn tagged_declared_length(bytes: &[u8], record_type: u16) -> Result<usize, BiometricError> {
    let prefix = format!("{record_type}.001:");
    if !bytes.starts_with(prefix.as_bytes()) {
        return Err(BiometricError::InvalidRecord);
    }
    let length_start = prefix.len();
    let length_end = bytes[length_start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map(|index| length_start + index)
        .ok_or(BiometricError::TruncatedRecord)?;
    if length_end == length_start {
        return Err(BiometricError::InvalidRecord);
    }
    parse_ascii_usize(&bytes[length_start..length_end])
}

fn tagged_record_type(bytes: &[u8]) -> Option<u16> {
    let dot = bytes.iter().position(|byte| *byte == b'.')?;
    if dot == 0 || dot > 2 || bytes.get(dot..dot + 5)? != b".001:" {
        return None;
    }
    std::str::from_utf8(&bytes[..dot]).ok()?.parse().ok()
}

fn validate_tagged_record(bytes: &[u8], record_type: u16) -> Result<(), BiometricError> {
    const MAX_TAGGED_FIELDS: usize = 1_024;

    if bytes.last() != Some(&FILE_SEPARATOR) {
        return Err(BiometricError::InvalidRecord);
    }
    let declared = tagged_declared_length(bytes, record_type)?;
    if declared != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }

    let record_prefix = format!("{record_type}.");
    let mut offset = 0_usize;
    let mut previous_field = 0_u16;
    let mut has_idc = record_type == 1;
    let mut field_count = 0_usize;
    while offset < bytes.len() - 1 {
        field_count += 1;
        if field_count > MAX_TAGGED_FIELDS || !bytes[offset..].starts_with(record_prefix.as_bytes())
        {
            return Err(BiometricError::InvalidRecord);
        }
        let number_start = offset + record_prefix.len();
        let number_end = number_start
            .checked_add(3)
            .ok_or(BiometricError::InvalidRecord)?;
        let number_bytes = bytes
            .get(number_start..number_end)
            .ok_or(BiometricError::TruncatedRecord)?;
        if !number_bytes.iter().all(u8::is_ascii_digit) || bytes.get(number_end) != Some(&b':') {
            return Err(BiometricError::InvalidRecord);
        }
        let field_number = parse_ascii_u16(number_bytes)?;
        if field_number <= previous_field || (field_count == 1 && field_number != 1) {
            return Err(BiometricError::InvalidRecord);
        }
        let value_start = number_end + 1;
        if field_number == 999 {
            if value_start >= bytes.len() - 1 {
                return Err(BiometricError::InvalidRecord);
            }
            offset = bytes.len() - 1;
        } else {
            let relative_end = bytes[value_start..]
                .iter()
                .position(|byte| *byte == GROUP_SEPARATOR || *byte == FILE_SEPARATOR)
                .ok_or(BiometricError::TruncatedRecord)?;
            let value_end = value_start + relative_end;
            if value_end == value_start {
                return Err(BiometricError::InvalidRecord);
            }
            if field_number == 2 && record_type != 1 {
                has_idc = parse_ascii_u16(&bytes[value_start..value_end])? <= 99;
            }
            if bytes[value_end] == FILE_SEPARATOR {
                if value_end != bytes.len() - 1 {
                    return Err(BiometricError::LengthMismatch);
                }
                offset = value_end;
            } else {
                offset = value_end + 1;
            }
        }
        previous_field = field_number;
    }
    if !has_idc || offset != bytes.len() - 1 {
        return Err(BiometricError::InvalidRecord);
    }
    Ok(())
}

fn validate_format(
    format: BiometricFormat,
    bytes: &[u8],
    explicit: bool,
) -> Result<(), BiometricError> {
    if format.permits_explicit_envelope_only() && !explicit {
        return Err(BiometricError::ExplicitFormatRequired);
    }
    match format {
        BiometricFormat::FingerprintAnsi378 | BiometricFormat::FingerprintIso19794_2 => {
            let detected = detect_fmr(bytes)?;
            if detected == format {
                Ok(())
            } else {
                Err(BiometricError::WrongFormat)
            }
        }
        BiometricFormat::FingerprintIso19794_4 => validate_fir(bytes, FirLayout::Iso2005),
        BiometricFormat::FingerprintAnsi381 => validate_fir(bytes, FirLayout::Ansi2004),
        BiometricFormat::FaceIso19794_5 => validate_fac_2005(bytes),
        BiometricFormat::IrisIso19794_6 => validate_iir(bytes),
        BiometricFormat::FingerprintAnsiNistType9 => validate_tagged_record(bytes, 9),
        BiometricFormat::FaceAnsiNistType10 => validate_tagged_record(bytes, 10),
        BiometricFormat::IrisAnsiNistType17 => validate_tagged_record(bytes, 17),
        BiometricFormat::FingerprintIso39794_2
        | BiometricFormat::FingerprintIso39794_4
        | BiometricFormat::FaceIso39794_5
        | BiometricFormat::IrisIso39794_6 => validate_asn1_container(bytes),
        BiometricFormat::FingerprintAnsiNistType4 => validate_ansi_nist_type_four(bytes),
        BiometricFormat::FingerprintCbeff
        | BiometricFormat::FaceCbeff
        | BiometricFormat::IrisCbeff => validate_cbeff_container(bytes, format.modality()),
    }
}

fn validate_ansi_nist_type_four(bytes: &[u8]) -> Result<(), BiometricError> {
    const HEADER_LENGTH: usize = 18;

    if bytes.len() <= HEADER_LENGTH {
        return Err(BiometricError::TruncatedRecord);
    }
    if read_u32_be(bytes, 0)? as usize != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    if bytes[4] > 99 || read_u16_be(bytes, 13)? == 0 || read_u16_be(bytes, 15)? == 0 {
        return Err(BiometricError::InvalidRecord);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum FirLayout {
    Ansi2004,
    Iso2005,
}

fn validate_fir(bytes: &[u8], layout: FirLayout) -> Result<(), BiometricError> {
    let (header_length, count_offset, scale_offset, resolution_offset, depth_offset) = match layout
    {
        FirLayout::Ansi2004 => (36, 22, 23, 24, 32),
        FirLayout::Iso2005 => (32, 18, 19, 20, 28),
    };
    if bytes.len() < header_length || !bytes.starts_with(b"FIR\0") {
        return Err(BiometricError::InvalidRecord);
    }
    if &bytes[4..8] != b"010\0" {
        return Err(BiometricError::UnsupportedVersion);
    }
    if read_u48_be(bytes, 8)? != bytes.len() as u64 {
        return Err(BiometricError::LengthMismatch);
    }
    let count = usize::from(bytes[count_offset]);
    if count == 0 || !matches!(bytes[scale_offset], 1 | 2) {
        return Err(BiometricError::InvalidRecord);
    }
    for offset in (resolution_offset..resolution_offset + 8).step_by(2) {
        if read_u16_be(bytes, offset)? == 0 {
            return Err(BiometricError::InvalidRecord);
        }
    }
    if !(1..=16).contains(&bytes[depth_offset]) || read_u16_be(bytes, depth_offset + 2)? != 0 {
        return Err(BiometricError::InvalidRecord);
    }

    let mut offset = header_length;
    for _ in 0..count {
        let header = bytes
            .get(offset..offset + 14)
            .ok_or(BiometricError::TruncatedRecord)?;
        let length = read_u32_be(header, 0)? as usize;
        if length < 14
            || header[5] == 0
            || header[6] == 0
            || header[6] > header[5]
            || read_u16_be(header, 9)? == 0
            || read_u16_be(header, 11)? == 0
            || header[13] != 0
        {
            return Err(BiometricError::InvalidRecord);
        }
        offset = checked_record_end(offset, length, bytes.len())?;
    }
    if offset != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(())
}

fn validate_fac_2005(bytes: &[u8]) -> Result<(), BiometricError> {
    const GENERAL_HEADER_LENGTH: usize = 14;
    const FACE_INFORMATION_LENGTH: usize = 20;
    const FEATURE_POINT_LENGTH: usize = 8;
    const IMAGE_INFORMATION_LENGTH: usize = 12;

    if bytes.len() < GENERAL_HEADER_LENGTH || !bytes.starts_with(b"FAC\0") {
        return Err(BiometricError::InvalidRecord);
    }
    if &bytes[4..8] != b"010\0" {
        return Err(BiometricError::UnsupportedVersion);
    }
    if read_u32_be(bytes, 8)? as usize != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    let count = usize::from(read_u16_be(bytes, 12)?);
    if count == 0 {
        return Err(BiometricError::InvalidRecord);
    }

    let mut offset = GENERAL_HEADER_LENGTH;
    for _ in 0..count {
        let block_length = read_u32_be(bytes, offset)? as usize;
        let feature_count = usize::from(read_u16_be(bytes, offset + 4)?);
        let feature_bytes = feature_count
            .checked_mul(FEATURE_POINT_LENGTH)
            .ok_or(BiometricError::InvalidRecord)?;
        let minimum = FACE_INFORMATION_LENGTH
            .checked_add(feature_bytes)
            .and_then(|value| value.checked_add(IMAGE_INFORMATION_LENGTH))
            .ok_or(BiometricError::InvalidRecord)?;
        if block_length < minimum {
            return Err(BiometricError::InvalidRecord);
        }
        let end = checked_record_end(offset, block_length, bytes.len())?;
        let image_information = offset
            .checked_add(FACE_INFORMATION_LENGTH)
            .and_then(|value| value.checked_add(feature_bytes))
            .ok_or(BiometricError::InvalidRecord)?;
        let image = bytes
            .get(image_information..image_information + IMAGE_INFORMATION_LENGTH)
            .ok_or(BiometricError::TruncatedRecord)?;
        if image[1] > 1 || read_u16_be(image, 2)? == 0 || read_u16_be(image, 4)? == 0 {
            return Err(BiometricError::InvalidRecord);
        }
        offset = end;
    }
    if offset != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(())
}

fn validate_iir(bytes: &[u8]) -> Result<(), BiometricError> {
    if bytes.len() < 8 || !bytes.starts_with(b"IIR\0") {
        return Err(BiometricError::InvalidRecord);
    }
    match &bytes[4..8] {
        b"010\0" => validate_iir_2005(bytes),
        b"020\0" => validate_iir_2011(bytes),
        _ => Err(BiometricError::UnsupportedVersion),
    }
}

fn validate_iir_2011(bytes: &[u8]) -> Result<(), BiometricError> {
    const GENERAL_HEADER_LENGTH: usize = 16;
    const REPRESENTATION_FIXED_LENGTH: usize = 52;
    const QUALITY_BLOCK_LENGTH: usize = 5;

    if bytes.len() < GENERAL_HEADER_LENGTH {
        return Err(BiometricError::TruncatedRecord);
    }
    if read_u32_be(bytes, 8)? as usize != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    let count = usize::from(read_u16_be(bytes, 12)?);
    if count == 0 || bytes[15] > 2 {
        return Err(BiometricError::InvalidRecord);
    }

    let mut offset = GENERAL_HEADER_LENGTH;
    for _ in 0..count {
        let representation_length = read_u32_be(bytes, offset)? as usize;
        let quality_count = usize::from(
            *bytes
                .get(offset + 18)
                .ok_or(BiometricError::TruncatedRecord)?,
        );
        let quality_bytes = quality_count
            .checked_mul(QUALITY_BLOCK_LENGTH)
            .ok_or(BiometricError::InvalidRecord)?;
        let fixed_length = REPRESENTATION_FIXED_LENGTH
            .checked_add(quality_bytes)
            .ok_or(BiometricError::InvalidRecord)?;
        if representation_length < fixed_length {
            return Err(BiometricError::InvalidRecord);
        }
        let image_length_offset = offset
            .checked_add(48)
            .and_then(|value| value.checked_add(quality_bytes))
            .ok_or(BiometricError::InvalidRecord)?;
        let image_length = read_u32_be(bytes, image_length_offset)? as usize;
        let expected_length = fixed_length
            .checked_add(image_length)
            .ok_or(BiometricError::InvalidRecord)?;
        if representation_length != expected_length {
            return Err(BiometricError::LengthMismatch);
        }
        let dimension_offset = offset
            .checked_add(25)
            .and_then(|value| value.checked_add(quality_bytes))
            .ok_or(BiometricError::InvalidRecord)?;
        if read_u16_be(bytes, dimension_offset)? == 0
            || read_u16_be(bytes, dimension_offset + 2)? == 0
        {
            return Err(BiometricError::InvalidRecord);
        }
        offset = checked_record_end(offset, representation_length, bytes.len())?;
    }
    if offset != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(())
}

fn validate_iir_2005(bytes: &[u8]) -> Result<(), BiometricError> {
    const GENERAL_HEADER_LENGTH: usize = 46;
    const IMAGE_HEADER_LENGTH: usize = 15;
    const ROI_MASK_LENGTH: usize = 3;
    const UNSEGMENTED_POLAR_LENGTH: usize = 12;
    const ANCILLARY_COORDINATE_LENGTH: usize = 24;
    const CHAIN_CODE_HEADER_LENGTH: usize = 6;

    if bytes.len() < GENERAL_HEADER_LENGTH {
        return Err(BiometricError::TruncatedRecord);
    }
    if read_u32_be(bytes, 9)? as usize != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    let eye_count = usize::from(bytes[15]);
    if eye_count > 2 || read_u16_be(bytes, 16)? as usize != GENERAL_HEADER_LENGTH {
        return Err(BiometricError::InvalidRecord);
    }

    let mut offset = GENERAL_HEADER_LENGTH;
    for _ in 0..eye_count {
        let image_count = usize::from(read_u16_be(bytes, offset + 1)?);
        if image_count == 0 {
            return Err(BiometricError::InvalidRecord);
        }
        offset = checked_record_end(offset, 3, bytes.len())?;
        for _ in 0..image_count {
            let image_length = read_u32_be(bytes, offset)? as usize;
            offset = checked_record_end(
                offset,
                IMAGE_HEADER_LENGTH + ROI_MASK_LENGTH + UNSEGMENTED_POLAR_LENGTH,
                bytes.len(),
            )?;
            offset = checked_record_end(offset, ANCILLARY_COORDINATE_LENGTH, bytes.len())?;
            for _ in 0..2 {
                let chain_length = usize::from(read_u16_be(bytes, offset)?);
                offset = checked_record_end(offset, 2, bytes.len())?;
                if chain_length != 0 {
                    if chain_length < CHAIN_CODE_HEADER_LENGTH {
                        return Err(BiometricError::InvalidRecord);
                    }
                    offset = checked_record_end(offset, chain_length, bytes.len())?;
                }
            }
            offset = checked_record_end(offset, image_length, bytes.len())?;
        }
    }
    if offset != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(())
}

fn checked_record_end(
    offset: usize,
    length: usize,
    record_length: usize,
) -> Result<usize, BiometricError> {
    let end = offset
        .checked_add(length)
        .ok_or(BiometricError::InvalidRecord)?;
    if end > record_length {
        return Err(BiometricError::TruncatedRecord);
    }
    Ok(end)
}

fn validate_asn1_container(bytes: &[u8]) -> Result<(), BiometricError> {
    // ISO/IEC 39794 tagged binary records are ASN.1 structures. The modality
    // schema is selected by the typed format envelope; this walk validates the
    // complete bounded TLV tree and rejects a merely length-prefixed blob.
    if bytes.first() != Some(&0x30) {
        return Err(BiometricError::InvalidRecord);
    }
    let consumed = validate_ber_tlv(bytes, 0)?;
    if consumed != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(())
}

fn validate_ber_tlv(bytes: &[u8], depth: usize) -> Result<usize, BiometricError> {
    const MAX_ASN1_DEPTH: usize = 64;
    const MAX_HIGH_TAG_BYTES: usize = 6;

    if depth > MAX_ASN1_DEPTH {
        return Err(BiometricError::InvalidRecord);
    }
    let first = *bytes.first().ok_or(BiometricError::TruncatedRecord)?;
    let mut tag_bytes = 1_usize;
    if first & 0x1F == 0x1F {
        loop {
            if tag_bytes > MAX_HIGH_TAG_BYTES {
                return Err(BiometricError::InvalidRecord);
            }
            let byte = *bytes
                .get(tag_bytes)
                .ok_or(BiometricError::TruncatedRecord)?;
            if tag_bytes == 1 && byte & 0x7F == 0 {
                return Err(BiometricError::InvalidRecord);
            }
            tag_bytes += 1;
            if byte & 0x80 == 0 {
                break;
            }
        }
    }
    let (content_length, length_bytes) = parse_ber_length(
        bytes
            .get(tag_bytes..)
            .ok_or(BiometricError::TruncatedRecord)?,
    )?;
    let content_start = tag_bytes
        .checked_add(length_bytes)
        .ok_or(BiometricError::InvalidRecord)?;
    let total = content_start
        .checked_add(content_length)
        .ok_or(BiometricError::InvalidRecord)?;
    let content = bytes
        .get(content_start..total)
        .ok_or(BiometricError::TruncatedRecord)?;
    if first & 0x20 != 0 {
        let mut offset = 0_usize;
        while offset < content.len() {
            offset = offset
                .checked_add(validate_ber_tlv(&content[offset..], depth + 1)?)
                .ok_or(BiometricError::InvalidRecord)?;
        }
        if offset != content.len() {
            return Err(BiometricError::LengthMismatch);
        }
    }
    Ok(total)
}

fn parse_ber_length(bytes: &[u8]) -> Result<(usize, usize), BiometricError> {
    let first = *bytes.first().ok_or(BiometricError::TruncatedRecord)?;
    if first & 0x80 == 0 {
        return Ok((usize::from(first), 1));
    }
    let count = usize::from(first & 0x7F);
    if count == 0 || count > std::mem::size_of::<usize>() {
        return Err(BiometricError::InvalidRecord);
    }
    let encoded = bytes
        .get(1..1 + count)
        .ok_or(BiometricError::TruncatedRecord)?;
    if encoded[0] == 0 {
        return Err(BiometricError::InvalidRecord);
    }
    let mut length = 0_usize;
    for byte in encoded {
        length = length
            .checked_mul(256)
            .and_then(|value| value.checked_add(usize::from(*byte)))
            .ok_or(BiometricError::RecordTooLarge {
                bytes: usize::MAX,
                maximum: MAX_BIOMETRIC_RECORD_BYTES,
            })?;
    }
    Ok((length, count + 1))
}

fn validate_cbeff_container(
    bytes: &[u8],
    expected_modality: BiometricModality,
) -> Result<(), BiometricError> {
    // CBEFF is a framework, not one self-identifying byte layout. RedDB admits
    // the public ANSI/NIST-ITL Type-99 patron carrier because its mandatory
    // SBH fields identify the biometric type and the BDB owner/type. An opaque
    // blob labelled merely "CBEFF" is not enough to validate safely.
    validate_tagged_record(bytes, 99)?;
    let (fields, data) = parse_type_99_fields(bytes)?;
    let required = |number| {
        fields
            .iter()
            .find_map(|(field, value)| (*field == number).then_some(*value))
            .ok_or(BiometricError::InvalidRecord)
    };

    let idc = required(2)?;
    if idc.is_empty()
        || idc.len() > 2
        || !idc.iter().all(u8::is_ascii_digit)
        || parse_ascii_u16(idc)? > 99
    {
        return Err(BiometricError::InvalidRecord);
    }
    if required(4)?.is_empty() {
        return Err(BiometricError::InvalidRecord);
    }
    let capture_date = required(5)?;
    if capture_date.len() != 8 || !capture_date.iter().all(u8::is_ascii_digit) {
        return Err(BiometricError::InvalidRecord);
    }
    if required(100)? != b"0101" {
        return Err(BiometricError::UnsupportedVersion);
    }
    if parse_fixed_hex(required(101)?, 8)? != cbeff_modality_code(expected_modality) {
        return Err(BiometricError::WrongModality);
    }
    let _format_owner = parse_fixed_hex(required(103)?, 4)?;
    let _format_type = parse_fixed_hex(required(104)?, 4)?;
    if data.is_empty() {
        return Err(BiometricError::InvalidRecord);
    }
    Ok(())
}

fn cbeff_modality(bytes: &[u8]) -> Result<BiometricModality, BiometricError> {
    validate_tagged_record(bytes, 99)?;
    let (fields, _) = parse_type_99_fields(bytes)?;
    let encoded = fields
        .iter()
        .find_map(|(field, value)| (*field == 101).then_some(*value))
        .ok_or(BiometricError::InvalidRecord)?;
    match parse_fixed_hex(encoded, 8)? {
        0x0000_0008 => Ok(BiometricModality::Fingerprint),
        0x0000_0002 => Ok(BiometricModality::Face),
        0x0000_0010 => Ok(BiometricModality::Iris),
        _ => Err(BiometricError::WrongModality),
    }
}

fn cbeff_modality_code(modality: BiometricModality) -> u32 {
    match modality {
        BiometricModality::Fingerprint => 0x0000_0008,
        BiometricModality::Face => 0x0000_0002,
        BiometricModality::Iris => 0x0000_0010,
    }
}

fn parse_fixed_hex(bytes: &[u8], width: usize) -> Result<u32, BiometricError> {
    if bytes.len() != width || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(BiometricError::InvalidRecord);
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .ok_or(BiometricError::InvalidRecord)
}

type Type99Fields<'a> = (Vec<(u32, &'a [u8])>, &'a [u8]);

fn parse_type_99_fields(bytes: &[u8]) -> Result<Type99Fields<'_>, BiometricError> {
    const MAX_TYPE_99_FIELDS: usize = 1_024;

    let mut fields = Vec::new();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        if fields.len() >= MAX_TYPE_99_FIELDS || !bytes[offset..].starts_with(b"99.") {
            return Err(BiometricError::InvalidRecord);
        }
        let number_start = offset + 3;
        let colon = bytes[number_start..]
            .iter()
            .position(|byte| *byte == b':')
            .map(|relative| number_start + relative)
            .ok_or(BiometricError::TruncatedRecord)?;
        if colon == number_start || colon - number_start > 9 {
            return Err(BiometricError::InvalidRecord);
        }
        let number = std::str::from_utf8(&bytes[number_start..colon])
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(BiometricError::InvalidRecord)?;
        let value_start = colon + 1;
        if number == 999 {
            let mut data = &bytes[value_start..];
            if data.last() == Some(&FILE_SEPARATOR) {
                data = &data[..data.len() - 1];
            }
            return Ok((fields, data));
        }
        let end = bytes[value_start..]
            .iter()
            .position(|byte| *byte == GROUP_SEPARATOR || *byte == FILE_SEPARATOR)
            .map(|relative| value_start + relative)
            .ok_or(BiometricError::TruncatedRecord)?;
        if fields.iter().any(|(field, _)| *field == number) {
            return Err(BiometricError::InvalidRecord);
        }
        fields.push((number, &bytes[value_start..end]));
        if bytes[end] == FILE_SEPARATOR {
            return Err(BiometricError::InvalidRecord);
        }
        offset = end + 1;
    }
    Err(BiometricError::TruncatedRecord)
}

#[derive(Clone, Copy)]
enum FmrLayout {
    Ansi2004,
    Iso2005,
    Ansi2009,
}

fn validate_fmr_layout(bytes: &[u8], layout: FmrLayout) -> Result<(), BiometricError> {
    let (declared, header_length, views_offset, ansi_2009) = match layout {
        FmrLayout::Iso2005 => (read_u32_be(bytes, 8)? as usize, 24, 22, false),
        FmrLayout::Ansi2004 => {
            let short = usize::from(read_u16_be(bytes, 8)?);
            if short == 0 {
                (read_u32_be(bytes, 10)? as usize, 30, 28, false)
            } else {
                (short, 26, 24, false)
            }
        }
        FmrLayout::Ansi2009 => (read_u32_be(bytes, 8)? as usize, 20, 18, true),
    };
    if bytes.len() < header_length {
        return Err(BiometricError::TruncatedRecord);
    }
    if declared != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    let views = usize::from(bytes[views_offset]);
    if views == 0 {
        return Err(BiometricError::InvalidRecord);
    }
    let mut offset = header_length;
    for _ in 0..views {
        let view_header = if ansi_2009 { 17 } else { 4 };
        let count_offset = if ansi_2009 { 16 } else { 3 };
        let header = bytes
            .get(offset..offset + view_header)
            .ok_or(BiometricError::TruncatedRecord)?;
        let minutiae = usize::from(header[count_offset]);
        offset = offset
            .checked_add(view_header)
            .and_then(|value| value.checked_add(minutiae.checked_mul(6)?))
            .ok_or(BiometricError::InvalidRecord)?;
        let extended = usize::from(read_u16_be(bytes, offset)?);
        offset = offset
            .checked_add(2)
            .and_then(|value| value.checked_add(extended))
            .ok_or(BiometricError::InvalidRecord)?;
        if offset > bytes.len() {
            return Err(BiometricError::TruncatedRecord);
        }
    }
    if offset != bytes.len() {
        return Err(BiometricError::LengthMismatch);
    }
    Ok(())
}

fn parse_fmr_views(bytes: &[u8], layout: FmrLayout) -> Result<Vec<MinutiaeView>, BiometricError> {
    validate_fmr_layout(bytes, layout)?;
    let (header_length, views_offset, ansi_2009, global_x_res, global_y_res) = match layout {
        FmrLayout::Iso2005 => (
            24,
            22,
            false,
            read_u16_be(bytes, 18)?,
            read_u16_be(bytes, 20)?,
        ),
        FmrLayout::Ansi2004 => {
            let large = read_u16_be(bytes, 8)? == 0;
            let shift = if large { 4 } else { 0 };
            (
                26 + shift,
                24 + shift,
                false,
                read_u16_be(bytes, 20 + shift)?,
                read_u16_be(bytes, 22 + shift)?,
            )
        }
        FmrLayout::Ansi2009 => (20, 18, true, 0, 0),
    };
    let count = usize::from(bytes[views_offset]);
    let mut result = Vec::with_capacity(count);
    let mut offset = header_length;
    for _ in 0..count {
        let header_length = if ansi_2009 { 17 } else { 4 };
        let header = &bytes[offset..offset + header_length];
        let position = header[0];
        let view = if ansi_2009 { header[1] } else { header[1] >> 4 };
        let minutiae_count = usize::from(header[if ansi_2009 { 16 } else { 3 }]);
        let (x_resolution, y_resolution) = if ansi_2009 {
            (
                u16::from_be_bytes([header[12], header[13]]),
                u16::from_be_bytes([header[14], header[15]]),
            )
        } else {
            (global_x_res, global_y_res)
        };
        if x_resolution == 0 || y_resolution == 0 {
            return Err(BiometricError::InvalidRecord);
        }
        offset += header_length;
        let mut minutiae = Vec::with_capacity(minutiae_count);
        for _ in 0..minutiae_count {
            let data = &bytes[offset..offset + 6];
            let x_field = u16::from_be_bytes([data[0], data[1]]);
            let y_field = u16::from_be_bytes([data[2], data[3]]);
            let raw_angle = u32::from(data[4]);
            let angle_microdegrees = match layout {
                FmrLayout::Iso2005 => u32::try_from(u64::from(raw_angle) * 360_000_000_u64 / 256)
                    .map_err(|_| BiometricError::InvalidRecord)?,
                FmrLayout::Ansi2004 | FmrLayout::Ansi2009 => raw_angle * 2_000_000,
            };
            minutiae.push(Minutia {
                x_micrometres: u32::from(x_field & 0x3FFF) * 10_000 / u32::from(x_resolution),
                y_micrometres: u32::from(y_field & 0x3FFF) * 10_000 / u32::from(y_resolution),
                angle_microdegrees,
                kind: (x_field >> 14) as u8,
                quality: data[5],
            });
            offset += 6;
        }
        let extended = usize::from(read_u16_be(bytes, offset)?);
        offset += 2 + extended;
        result.push(MinutiaeView {
            position,
            view,
            minutiae,
        });
    }
    Ok(result)
}

fn read_u16_be(bytes: &[u8], offset: usize) -> Result<u16, BiometricError> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or(BiometricError::TruncatedRecord)?;
    Ok(u16::from_be_bytes([data[0], data[1]]))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, BiometricError> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or(BiometricError::TruncatedRecord)?;
    Ok(u32::from_be_bytes([data[0], data[1], data[2], data[3]]))
}

fn read_u48_be(bytes: &[u8], offset: usize) -> Result<u64, BiometricError> {
    let data = bytes
        .get(offset..offset + 6)
        .ok_or(BiometricError::TruncatedRecord)?;
    let mut value = 0_u64;
    for byte in data {
        value = (value << 8) | u64::from(*byte);
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiometricError {
    EmptyRecord,
    RecordTooLarge { bytes: usize, maximum: usize },
    StandaloneImage,
    UnknownFormat,
    AmbiguousFormat,
    ExplicitFormatRequired,
    WrongModality,
    WrongFormat,
    UnsupportedVersion,
    TruncatedRecord,
    LengthMismatch,
    InvalidRecord,
    MinutiaeTemplateRequired,
    InvalidOccurrence,
    RecordNotFound,
    UnsupportedRecordType,
}

impl fmt::Display for BiometricError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRecord => write!(f, "biometric record cannot be empty"),
            Self::RecordTooLarge { bytes, maximum } => {
                write!(
                    f,
                    "biometric record has {bytes} bytes; maximum is {maximum}"
                )
            }
            Self::StandaloneImage => write!(f, "standalone biometric images are not accepted"),
            Self::UnknownFormat => write!(f, "biometric record format is not recognized"),
            Self::AmbiguousFormat => write!(f, "biometric record format is ambiguous"),
            Self::ExplicitFormatRequired => {
                write!(
                    f,
                    "biometric record requires an explicit typed format envelope"
                )
            }
            Self::WrongModality => write!(f, "biometric record has a different modality"),
            Self::WrongFormat => write!(f, "biometric record does not match its declared format"),
            Self::UnsupportedVersion => write!(f, "biometric record version is not supported"),
            Self::TruncatedRecord => write!(f, "biometric record is truncated"),
            Self::LengthMismatch => write!(f, "biometric record length fields are inconsistent"),
            Self::InvalidRecord => write!(f, "biometric record is structurally invalid"),
            Self::MinutiaeTemplateRequired => {
                write!(f, "fingerprint comparison requires a minutiae template")
            }
            Self::InvalidOccurrence => write!(f, "ANSI/NIST record occurrence must be positive"),
            Self::RecordNotFound => write!(f, "requested ANSI/NIST record was not found"),
            Self::UnsupportedRecordType => {
                write!(
                    f,
                    "ANSI/NIST record type is not a supported biometric modality"
                )
            }
        }
    }
}

impl std::error::Error for BiometricError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso_fmr() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FMR\0 20\0");
        bytes.extend_from_slice(&36_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&[1, 0, 100, 1]);
        bytes.extend_from_slice(&[0x40, 1, 0, 2, 3, 90]);
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    fn ansi_fmr() -> Vec<u8> {
        let mut bytes = vec![0; 38];
        bytes[..8].copy_from_slice(b"FMR\0 20\0");
        bytes[8..10].copy_from_slice(&38_u16.to_be_bytes());
        bytes[20..22].copy_from_slice(&197_u16.to_be_bytes());
        bytes[22..24].copy_from_slice(&197_u16.to_be_bytes());
        bytes[24] = 1;
        bytes[26..30].copy_from_slice(&[1, 0, 100, 1]);
        bytes[30..36].copy_from_slice(&[0x40, 1, 0, 2, 3, 90]);
        bytes
    }

    fn fir(layout: FirLayout) -> Vec<u8> {
        let (header_length, count_offset, scale_offset, resolution_offset, depth_offset) =
            match layout {
                FirLayout::Ansi2004 => (36, 22, 23, 24, 32),
                FirLayout::Iso2005 => (32, 18, 19, 20, 28),
            };
        let length = header_length + 14;
        let mut bytes = vec![0; length];
        bytes[..8].copy_from_slice(b"FIR\0010\0");
        let encoded_length = (length as u64).to_be_bytes();
        bytes[8..14].copy_from_slice(&encoded_length[2..]);
        bytes[count_offset] = 1;
        bytes[scale_offset] = 1;
        for offset in (resolution_offset..resolution_offset + 8).step_by(2) {
            bytes[offset..offset + 2].copy_from_slice(&500_u16.to_be_bytes());
        }
        bytes[depth_offset] = 8;
        bytes[header_length..header_length + 4].copy_from_slice(&14_u32.to_be_bytes());
        bytes[header_length + 4..header_length + 9].copy_from_slice(&[1, 1, 1, 100, 0]);
        bytes[header_length + 9..header_length + 11].copy_from_slice(&1_u16.to_be_bytes());
        bytes[header_length + 11..header_length + 13].copy_from_slice(&1_u16.to_be_bytes());
        bytes
    }

    fn fac_2005() -> Vec<u8> {
        let mut bytes = vec![0; 46];
        bytes[..8].copy_from_slice(b"FAC\0010\0");
        bytes[8..12].copy_from_slice(&46_u32.to_be_bytes());
        bytes[12..14].copy_from_slice(&1_u16.to_be_bytes());
        bytes[14..18].copy_from_slice(&32_u32.to_be_bytes());
        bytes[36..38].copy_from_slice(&1_u16.to_be_bytes());
        bytes[38..40].copy_from_slice(&1_u16.to_be_bytes());
        bytes
    }

    fn fac_2005_landmarks(points: &[(u8, u16, u16)]) -> Vec<u8> {
        let block_length = 20 + points.len() * 8 + 12;
        let mut bytes = vec![0; 14 + block_length];
        bytes[..8].copy_from_slice(b"FAC\0010\0");
        let total = u32::try_from(bytes.len()).expect("bounded fixture");
        bytes[8..12].copy_from_slice(&total.to_be_bytes());
        bytes[12..14].copy_from_slice(&1_u16.to_be_bytes());
        bytes[14..18].copy_from_slice(
            &u32::try_from(block_length)
                .expect("bounded fixture")
                .to_be_bytes(),
        );
        bytes[18..20].copy_from_slice(
            &u16::try_from(points.len())
                .expect("bounded fixture")
                .to_be_bytes(),
        );
        for (index, (code, x, y)) in points.iter().enumerate() {
            let offset = 34 + index * 8;
            bytes[offset] = 1;
            bytes[offset + 1] = *code;
            bytes[offset + 2..offset + 4].copy_from_slice(&x.to_be_bytes());
            bytes[offset + 4..offset + 6].copy_from_slice(&y.to_be_bytes());
        }
        let image = 34 + points.len() * 8;
        bytes[image + 2..image + 4].copy_from_slice(&1_u16.to_be_bytes());
        bytes[image + 4..image + 6].copy_from_slice(&1_u16.to_be_bytes());
        bytes
    }

    fn iir_2005() -> Vec<u8> {
        let mut bytes = vec![0; 107];
        bytes[..8].copy_from_slice(b"IIR\0010\0");
        bytes[8] = 1;
        bytes[9..13].copy_from_slice(&107_u32.to_be_bytes());
        bytes[15] = 1;
        bytes[16..18].copy_from_slice(&46_u16.to_be_bytes());
        bytes[22..24].copy_from_slice(&2_u16.to_be_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_be_bytes());
        bytes[26..28].copy_from_slice(&1_u16.to_be_bytes());
        bytes[28] = 8;
        bytes[30..46].copy_from_slice(b"0000000000000000");
        bytes[46] = 1;
        bytes[47..49].copy_from_slice(&1_u16.to_be_bytes());
        bytes
    }

    fn iir_2011() -> Vec<u8> {
        let mut bytes = vec![0; 68];
        bytes[..8].copy_from_slice(b"IIR\0020\0");
        bytes[8..12].copy_from_slice(&68_u32.to_be_bytes());
        bytes[12..14].copy_from_slice(&1_u16.to_be_bytes());
        bytes[15] = 1;
        bytes[16..20].copy_from_slice(&52_u32.to_be_bytes());
        bytes[41..43].copy_from_slice(&1_u16.to_be_bytes());
        bytes[43..45].copy_from_slice(&1_u16.to_be_bytes());
        bytes[45] = 8;
        bytes
    }

    fn tagged_record(record_type: u16) -> Vec<u8> {
        let mut record = format!("{record_type}.001:0\x1d{record_type}.002:00\x1c").into_bytes();
        finalize_tagged_length(&mut record);
        record
    }

    fn type_99(modality_code: &str) -> Vec<u8> {
        let mut record = format!("99.001:0\x1d99.002:00\x1d99.004:agency\x1d99.005:20261113\x1d99.100:0101\x1d99.101:{modality_code}\x1d99.103:0101\x1d99.104:0008\x1d99.999:bdb\x1c").into_bytes();
        finalize_tagged_length(&mut record);
        record
    }

    fn finalize_tagged_length(record: &mut Vec<u8>) {
        loop {
            let marker_end = record
                .iter()
                .position(|byte| *byte == GROUP_SEPARATOR)
                .expect("fixture has first separator");
            let record_type = tagged_record_type(record).expect("fixture has record type");
            let replacement = format!("{record_type}.001:{}", record.len()).into_bytes();
            let mut updated = replacement;
            updated.extend_from_slice(&record[marker_end..]);
            if updated == *record {
                return;
            }
            *record = updated;
        }
    }

    #[test]
    fn detects_and_validates_iso_minutiae() {
        let sample =
            BiometricSample::detect(BiometricModality::Fingerprint, iso_fmr()).expect("valid FMR");
        assert_eq!(sample.format(), BiometricFormat::FingerprintIso19794_2);
    }

    #[test]
    fn detects_ansi_and_iso_binary_fingerprint_layouts() {
        let ansi = BiometricSample::detect(BiometricModality::Fingerprint, ansi_fmr())
            .expect("valid ANSI 378");
        assert_eq!(ansi.format(), BiometricFormat::FingerprintAnsi378);

        let ansi =
            BiometricSample::detect(BiometricModality::Fingerprint, fir(FirLayout::Ansi2004))
                .expect("valid ANSI 381");
        assert_eq!(ansi.format(), BiometricFormat::FingerprintAnsi381);

        let iso = BiometricSample::detect(BiometricModality::Fingerprint, fir(FirLayout::Iso2005))
            .expect("valid ISO 19794-4");
        assert_eq!(iso.format(), BiometricFormat::FingerprintIso19794_4);
    }

    #[test]
    fn validates_face_and_both_iso_iris_binary_editions() {
        assert_eq!(
            BiometricSample::detect(BiometricModality::Face, fac_2005())
                .expect("valid face record")
                .format(),
            BiometricFormat::FaceIso19794_5
        );
        for bytes in [iir_2005(), iir_2011()] {
            assert_eq!(
                BiometricSample::detect(BiometricModality::Iris, bytes)
                    .expect("valid iris record")
                    .format(),
                BiometricFormat::IrisIso19794_6
            );
        }
    }

    #[test]
    fn decodes_known_iso_face_landmarks_without_image_extraction() {
        let bytes = fac_2005_landmarks(&[
            (0xc1, 10, 20),
            (0xc2, 30, 20),
            (0xc3, 14, 30),
            (0xc4, 26, 30),
            (0x9f, 20, 36),
        ]);
        let sample =
            BiometricSample::detect(BiometricModality::Face, bytes).expect("valid face record");
        let landmarks = sample.face_landmarks().expect("known topology");
        assert_eq!(
            landmarks.space_id(),
            crate::ISO_19794_5_2005_LANDMARK_SPACE_ID
        );
        assert_eq!(landmarks.dimensions(), 2);
        assert_eq!(landmarks.pairs().len(), 2);
        assert_eq!(landmarks.midline().len(), 1);
        assert_eq!(landmarks.symmetry_score(), Ok(1.0));
    }

    #[test]
    fn face_landmarks_reject_unknown_duplicate_and_incomplete_topology() {
        for (points, error) in [
            (
                vec![
                    (0xc1, 10, 20),
                    (0xc2, 30, 20),
                    (0xc3, 14, 30),
                    (0xfe, 26, 30),
                ],
                crate::LandmarkError::UnknownFeatureCode,
            ),
            (
                vec![
                    (0xc1, 10, 20),
                    (0xc1, 30, 20),
                    (0xc3, 14, 30),
                    (0xc4, 26, 30),
                ],
                crate::LandmarkError::DuplicateFeatureCode,
            ),
            (
                vec![(0xc1, 10, 20), (0xc2, 30, 20), (0xc3, 14, 30)],
                crate::LandmarkError::IncompleteTopology,
            ),
        ] {
            let sample =
                BiometricSample::detect(BiometricModality::Face, fac_2005_landmarks(&points))
                    .expect("structurally valid face record");
            assert_eq!(sample.face_landmarks(), Err(error));
        }
    }

    #[test]
    fn face_landmarks_reject_truncation_overflow_and_multiple_faces() {
        let valid = fac_2005_landmarks(&[
            (0xc1, 10, 20),
            (0xc2, 30, 20),
            (0xc3, 14, 30),
            (0xc4, 26, 30),
        ]);
        assert_eq!(
            decode_fac_2005_landmarks(&valid[..valid.len() - 1]),
            Err(crate::LandmarkError::TruncatedLandmarkBlock)
        );

        let mut overflow = valid.clone();
        overflow[14..18].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode_fac_2005_landmarks(&overflow),
            Err(crate::LandmarkError::TruncatedLandmarkBlock)
        );

        let mut multiple = valid;
        multiple[12..14].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            decode_fac_2005_landmarks(&multiple),
            Err(crate::LandmarkError::AmbiguousFaceRecord)
        );
    }

    #[test]
    fn binary_image_records_reject_truncation_and_declared_length_overflow() {
        let fixtures = [
            (BiometricModality::Fingerprint, fir(FirLayout::Iso2005)),
            (BiometricModality::Fingerprint, fir(FirLayout::Ansi2004)),
            (BiometricModality::Face, fac_2005()),
            (BiometricModality::Iris, iir_2005()),
            (BiometricModality::Iris, iir_2011()),
        ];
        for (modality, bytes) in fixtures {
            for length in 0..bytes.len() {
                assert!(BiometricSample::detect(modality, bytes[..length].to_vec()).is_err());
            }
        }

        let mut bytes = fir(FirLayout::Iso2005);
        bytes[8..14].fill(u8::MAX);
        assert!(BiometricSample::detect(BiometricModality::Fingerprint, bytes).is_err());

        let mut bytes = iir_2005();
        bytes[103..105].copy_from_slice(&5_u16.to_be_bytes());
        assert!(BiometricSample::detect(BiometricModality::Iris, bytes).is_err());
    }

    #[test]
    fn decodes_full_iso_angle_range_without_overflow() {
        let mut bytes = iso_fmr();
        bytes[32] = u8::MAX;
        let sample =
            BiometricSample::detect(BiometricModality::Fingerprint, bytes).expect("valid FMR");
        let views = sample.minutiae_views().expect("decoded minutiae");
        assert_eq!(views[0].minutiae[0].angle_microdegrees, 358_593_750);
    }

    #[test]
    fn rejects_raw_images_without_exposing_payload() {
        let error =
            BiometricSample::detect(BiometricModality::Face, vec![0xFF, 0xD8, 0xFF, 1, 2, 3])
                .expect_err("raw JPEG must fail");
        assert_eq!(error, BiometricError::StandaloneImage);
        assert!(!error.to_string().contains("255"));
    }

    #[test]
    fn rejects_truncated_fmr_before_allocating_from_counts() {
        let mut bytes = iso_fmr();
        bytes.pop();
        assert!(BiometricSample::detect(BiometricModality::Fingerprint, bytes).is_err());
    }

    #[test]
    fn selects_binary_type_four_from_type_one_directory() {
        let mut type_one = b"1.001:0\x1d1.002:0500\x1d1.003:1\x1f1\x1e4\x1f00\x1c".to_vec();
        loop {
            let marker_end = type_one
                .iter()
                .position(|byte| *byte == GROUP_SEPARATOR)
                .expect("fixture has first separator");
            let replacement = format!("1.001:{}", type_one.len()).into_bytes();
            let mut updated = replacement;
            updated.extend_from_slice(&type_one[marker_end..]);
            if updated == type_one {
                break;
            }
            type_one = updated;
        }
        let mut type_four = vec![0; 19];
        type_four[..4].copy_from_slice(&19_u32.to_be_bytes());
        type_four[13..15].copy_from_slice(&1_u16.to_be_bytes());
        type_four[15..17].copy_from_slice(&1_u16.to_be_bytes());
        let mut transaction = type_one;
        transaction.extend_from_slice(&type_four);

        assert_eq!(
            select_ansi_nist_record(&transaction, 4, 1),
            Ok(type_four.clone())
        );
        let sample = select_ansi_nist_sample(&transaction, 4, 1).expect("typed sample");
        assert_eq!(sample.format(), BiometricFormat::FingerprintAnsiNistType4);
    }

    #[test]
    fn cbeff_requires_a_valid_type_99_patron_record() {
        let bytes = type_99("00000002");
        let sample = BiometricSample::detect(BiometricModality::Face, bytes.clone())
            .expect("valid Type-99 CBEFF record");
        assert_eq!(sample.format(), BiometricFormat::FaceCbeff);
        assert_eq!(
            select_ansi_nist_sample(&bytes, 99, 1)
                .expect("selected CBEFF sample")
                .format(),
            BiometricFormat::FaceCbeff
        );
        assert_eq!(
            BiometricSample::detect(BiometricModality::Iris, bytes),
            Err(BiometricError::WrongModality)
        );
        assert!(BiometricSample::with_format(BiometricFormat::FaceCbeff, vec![0; 8]).is_err());
    }

    #[test]
    fn detects_tagged_ansi_nist_and_cbeff_records_for_each_modality() {
        for (modality, record_type, format) in [
            (
                BiometricModality::Fingerprint,
                9,
                BiometricFormat::FingerprintAnsiNistType9,
            ),
            (
                BiometricModality::Face,
                10,
                BiometricFormat::FaceAnsiNistType10,
            ),
            (
                BiometricModality::Iris,
                17,
                BiometricFormat::IrisAnsiNistType17,
            ),
        ] {
            assert_eq!(
                BiometricSample::detect(modality, tagged_record(record_type))
                    .expect("valid tagged record")
                    .format(),
                format
            );
        }
        for (modality, code, format) in [
            (
                BiometricModality::Fingerprint,
                "00000008",
                BiometricFormat::FingerprintCbeff,
            ),
            (
                BiometricModality::Face,
                "00000002",
                BiometricFormat::FaceCbeff,
            ),
            (
                BiometricModality::Iris,
                "00000010",
                BiometricFormat::IrisCbeff,
            ),
        ] {
            assert_eq!(
                BiometricSample::detect(modality, type_99(code))
                    .expect("valid Type-99 carrier")
                    .format(),
                format
            );
        }
    }

    #[test]
    fn ansi_nist_transaction_with_multiple_matching_records_requires_selection() {
        let record = tagged_record(9);
        let mut type_one =
            b"1.001:0\x1d1.002:0500\x1d1.003:1\x1f2\x1e9\x1f00\x1e9\x1f01\x1c".to_vec();
        finalize_tagged_length(&mut type_one);
        let mut transaction = type_one;
        transaction.extend_from_slice(&record);
        transaction.extend_from_slice(&record);

        assert_eq!(
            BiometricSample::detect(BiometricModality::Fingerprint, transaction.clone()),
            Err(BiometricError::AmbiguousFormat)
        );
        assert_eq!(select_ansi_nist_record(&transaction, 9, 2), Ok(record));
    }

    #[test]
    fn iso_39794_envelope_requires_a_complete_asn1_sequence() {
        let encoded = vec![0x30, 0x03, 0x02, 0x01, 0x01];
        for format in [
            BiometricFormat::FingerprintIso39794_2,
            BiometricFormat::FingerprintIso39794_4,
            BiometricFormat::FaceIso39794_5,
            BiometricFormat::IrisIso39794_6,
        ] {
            assert!(BiometricSample::with_format(format, encoded.clone()).is_ok());
        }
        assert_eq!(
            BiometricSample::with_format(BiometricFormat::FaceIso39794_5, encoded[..4].to_vec()),
            Err(BiometricError::TruncatedRecord)
        );
        assert_eq!(
            BiometricSample::with_format(BiometricFormat::FaceIso39794_5, vec![0x02, 0x01, 0x01]),
            Err(BiometricError::InvalidRecord)
        );
    }

    #[test]
    fn debug_output_does_not_expose_biometric_payloads() {
        let sample = BiometricSample::with_format(
            BiometricFormat::FaceIso39794_5,
            vec![0x30, 0x03, 0x02, 0x01, 0x01],
        )
        .expect("valid record");
        let debug = format!("{sample:?}");
        assert!(debug.contains("bytes: 5"));
        assert!(!debug.contains("48, 3, 2, 1, 1"));
    }

    proptest::proptest! {
        #[test]
        fn bounded_arbitrary_records_never_panic(
            modality in 1_u8..=3,
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..2_048),
        ) {
            let modality = match modality {
                1 => BiometricModality::Fingerprint,
                2 => BiometricModality::Face,
                _ => BiometricModality::Iris,
            };
            let _ = BiometricSample::detect(modality, bytes);
        }
    }
}
