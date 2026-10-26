//! Platform-independent UTF-16 construction used by the native Windows adapter.
use crate::protocol::{ApiError, ErrorCode};
use std::cmp::Ordering;
pub type EnvironmentEntry = (Vec<u16>, Vec<u16>);
pub type Environment = Vec<EnvironmentEntry>;

fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}

/// Quote one CRT-style argv value, preserving embedded quotes and trailing slashes.
pub fn quote_arg(arg: &[u16]) -> Result<Vec<u16>, ApiError> {
    if arg.contains(&0) {
        return Err(invalid("Windows argv cannot contain NUL"));
    }
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for &unit in arg {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if unit == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        }
        slashes = 0;
        out.push(unit);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    Ok(out)
}
pub fn command_line(executable: &[u16], args: &[Vec<u16>]) -> Result<Vec<u16>, ApiError> {
    let mut line = quote_arg(executable)?;
    for arg in args {
        line.push(b' ' as u16);
        line.extend(quote_arg(arg)?);
    }
    if line.len() >= 32767 {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Windows command line exceeds 32767 UTF-16 units including terminator",
        ));
    }
    line.push(0);
    Ok(line)
}
/// Last override wins under the OS's case-insensitive ordinal key comparison.
/// A fallible comparator lets native CompareStringOrdinal errors remain explicit.
pub fn merge_environment<F>(
    mut inherited: Environment,
    overrides: Environment,
    mut compare: F,
) -> Result<Environment, ApiError>
where
    F: FnMut(&[u16], &[u16]) -> Result<Ordering, ApiError>,
{
    for entry in overrides {
        let mut replaced = false;
        for current in &mut inherited {
            if compare(&current.0, &entry.0)? == Ordering::Equal {
                *current = entry.clone();
                replaced = true;
                break;
            }
        }
        if !replaced {
            inherited.push(entry);
        }
    }
    // Sort with fallible comparisons rather than discarding an OS comparison error.
    for i in 1..inherited.len() {
        let mut j = i;
        while j > 0 && compare(&inherited[j - 1].0, &inherited[j].0)? == Ordering::Greater {
            inherited.swap(j - 1, j);
            j -= 1;
        }
    }
    Ok(inherited)
}
pub fn environment_block(entries: &[EnvironmentEntry]) -> Result<Vec<u16>, ApiError> {
    let mut block = Vec::new();
    for (key, value) in entries {
        // Match inherited_environment: the separator is the first '=' after
        // the initial code unit, so hidden names are not limited to '=C:'.
        if key.is_empty()
            || key.contains(&0)
            || key.iter().skip(1).any(|&unit| unit == b'=' as u16)
            || value.contains(&0)
        {
            let name: String = String::from_utf16_lossy(key).chars().take(128).collect();
            return Err(invalid(&format!(
                "invalid Windows environment key or value for variable {name:?}"
            )));
        }
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}
