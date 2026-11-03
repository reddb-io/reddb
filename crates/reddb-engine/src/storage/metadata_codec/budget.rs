//! Allocation preflight for the existing metadata grammar. No payload is cloned
//! and nesting uses a fixed explicit stack before the typed decoder allocates.
use super::*;
use reddb_file::{native_value::DecodeBudget, RdbFileError, RdbFileResult};

pub(super) fn admit(data: &[u8], budget: &mut DecodeBudget) -> RdbFileResult<()> {
    let mut position = 0;
    let count = read_count(data, &mut position, 5).map_err(error)?;
    budget.elements::<Metadata>(1)?;
    budget.map::<String, MetadataValue>(count)?;
    const LEVELS: usize = reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize + 2;
    let mut frames = [(0_usize, false); LEVELS];
    frames[0] = (count, true);
    let mut depth = 1;
    while depth > 0 {
        let frame = &mut frames[depth - 1];
        if frame.0 == 0 {
            depth -= 1;
            continue;
        }
        frame.0 -= 1;
        if frame.1 {
            string(data, &mut position, budget)?;
        }
        let tag = read_u8(data, &mut position).map_err(error)?;
        let children = match tag {
            0 => None,
            1 => {
                read_exact_slice(data, &mut position, 1).map_err(error)?;
                None
            }
            2 | 3 | 8 => {
                read_exact_slice(data, &mut position, 8).map_err(error)?;
                None
            }
            4 | 5 => {
                string(data, &mut position, budget)?;
                None
            }
            6 => {
                let count = read_count(data, &mut position, 1).map_err(error)?;
                budget.elements::<MetadataValue>(count)?;
                Some((count, false))
            }
            7 => {
                let count = read_count(data, &mut position, 5).map_err(error)?;
                budget.map::<String, MetadataValue>(count)?;
                Some((count, true))
            }
            9 => {
                read_exact_slice(data, &mut position, 16).map_err(error)?;
                None
            }
            10 => {
                reference(data, &mut position, budget)?;
                None
            }
            11 => {
                let count = read_count(data, &mut position, 13).map_err(error)?;
                budget.elements::<super::super::metadata::RefTarget>(count)?;
                for _ in 0..count {
                    reference(data, &mut position, budget)?;
                }
                None
            }
            _ => return Err(error("unknown metadata value tag")),
        };
        if let Some((count, keys)) = children.filter(|(count, _)| *count > 0) {
            if depth >= frames.len() {
                return Err(error("metadata nesting exceeds the maximum depth"));
            }
            frames[depth] = (count, keys);
            depth += 1;
        }
    }
    Ok(())
}

fn error(message: impl std::fmt::Display) -> RdbFileError {
    RdbFileError::InvalidOperation(message.to_string())
}

fn string(data: &[u8], position: &mut usize, budget: &mut DecodeBudget) -> RdbFileResult<()> {
    let length = read_u32(data, position).map_err(error)? as usize;
    read_exact_slice(data, position, length).map_err(error)?;
    budget.charge(length)
}

fn reference(data: &[u8], position: &mut usize, budget: &mut DecodeBudget) -> RdbFileResult<()> {
    if read_u8(data, position).map_err(error)? > 4 {
        return Err(error("unknown metadata reference tag"));
    }
    string(data, position, budget)?;
    read_exact_slice(data, position, 8).map_err(error)?;
    Ok(())
}
