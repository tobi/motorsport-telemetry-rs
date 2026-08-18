//! Low-level JSON helpers shared by the reader and writer.

use crate::write::TelemetryFormatError;
use serde_json::{Map, Number, Value};

pub(super) fn invalid(message: impl Into<String>) -> TelemetryFormatError {
    TelemetryFormatError::Invalid(message.into())
}
pub(super) fn parse_json(line: &str) -> Result<Value, TelemetryFormatError> {
    serde_json::from_str(line).map_err(|err| invalid(err.to_string()))
}
pub(super) fn next_record(
    lines: &mut impl Iterator<Item = std::io::Result<String>>,
    what: &str,
) -> Result<String, TelemetryFormatError> {
    let line = lines
        .next()
        .ok_or_else(|| invalid(format!("missing {what} record")))??;
    if line.is_empty() {
        return Err(invalid(format!("{what} record is empty")));
    }
    Ok(line)
}
pub(super) fn int_field(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, TelemetryFormatError> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => {
            Ok(Some(json_u64(value)?.ok_or_else(|| {
                invalid(format!("{key} must be an integer"))
            })?))
        }
    }
}
pub(super) fn string_field(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}
pub(super) fn json_u64(value: &Value) -> Result<Option<u64>, TelemetryFormatError> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_i64().and_then(|value| u64::try_from(value).ok()))
            .map(Some)
            .ok_or_else(|| invalid("expected a non-negative integer")),
        Value::Null => Ok(None),
        _ => Err(invalid("expected a non-negative integer")),
    }
}
pub(super) fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            number.as_f64().and_then(|value| {
                (value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64)
                    .then_some(value as i64)
            })
        }),
        _ => None,
    }
}
pub(super) fn json_complete(value: &Value) -> Result<bool, TelemetryFormatError> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        Value::Number(number) if number.as_u64() == Some(1) || as_one(number) => Ok(true),
        Value::Number(number) if number.as_u64() == Some(0) || as_zero(number) => Ok(false),
        _ => Err(invalid("lap complete must be 0 or 1")),
    }
}
pub(super) fn as_one(number: &Number) -> bool {
    number.as_i64() == Some(1) || number.as_f64() == Some(1.0)
}
pub(super) fn as_zero(number: &Number) -> bool {
    number.as_i64() == Some(0) || number.as_f64() == Some(0.0)
}
pub(super) fn json_finite(
    value: &Value,
    name: &str,
    what: &str,
) -> Result<f64, TelemetryFormatError> {
    let number = value
        .as_f64()
        .ok_or_else(|| invalid(format!("channel {name} {what} must be a number")))?;
    if !number.is_finite() {
        return Err(invalid(format!("channel {name} {what} must be finite")));
    }
    Ok(number)
}
