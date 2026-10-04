//! Descriptive file metadata: merge rules shared by views and file loaders.
//!
//! Values are JSON-compatible so a resolved YAML document can travel in an
//! MTJ header without importing YAML or filesystem policy into the readers.

use crate::FileMetadata;
use serde_json::Value;

/// Arbitrary structured file metadata, with string keys and JSON values.
pub type MetadataMap = serde_json::Map<String, Value>;

/// Recursively overlays maps, replacing other values (including arrays).
///
/// Missing keys inherit; blank strings are ignored, as in Omatrack. `null`
/// remains an explicit mask rather than deleting a key and exposing a fallback.
pub fn merge_metadata(base: &mut MetadataMap, overlay: &MetadataMap) {
    for (key, value) in overlay {
        match value {
            Value::String(text) if text.trim().is_empty() => {}
            Value::Object(fields) => {
                let target = base
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(MetadataMap::new()));
                if !target.is_object() {
                    *target = Value::Object(MetadataMap::new());
                }
                if let Some(target) = target.as_object_mut() {
                    merge_metadata(target, fields);
                }
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Looks up a field while preserving a null mask on any ancestor.
fn field<'a>(map: &'a MetadataMap, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let value = map.get(*first)?;
    if rest.is_empty() || value.is_null() {
        Some(value)
    } else {
        field(value.as_object()?, rest)
    }
}

fn apply_text(target: &mut String, value: Option<&Value>) {
    match value {
        Some(Value::Null) => target.clear(),
        Some(Value::String(text)) if !text.trim().is_empty() => target.clone_from(text),
        Some(Value::Number(number)) => *target = number.to_string(),
        _ => {}
    }
}

impl FileMetadata {
    /// Projects descriptive fields from [`Self::extra`] onto the identity.
    ///
    /// Call after deriving source clocks: a civil date or track override must
    /// never move timestamps or change session grouping. `archive.event_date`
    /// is the event date, not the recording date, and stays in `extra`.
    pub fn apply_extra_metadata(&mut self) {
        // Rebuild, never apply onto an identity already projected from an older
        // overlay. Clearing an embedded map must not leave its old name behind.
        self.identity.clone_from(&self.source_identity);
        apply_text(
            &mut self.identity.driver,
            field(&self.extra, &["driver", "name"]),
        );
        apply_text(
            &mut self.identity.vehicle,
            field(&self.extra, &["car", "name"]),
        );
        apply_text(
            &mut self.identity.venue,
            field(&self.extra, &["track", "name"]),
        );
        apply_text(&mut self.identity.event, self.extra.get("event"));
        apply_text(&mut self.identity.session, self.extra.get("session"));
        apply_text(&mut self.identity.date, self.extra.get("date"));
        apply_text(&mut self.identity.time, self.extra.get("time"));
    }

    /// Resolves a positive numeric driver code through `driver.mappings`.
    ///
    /// Exact numeric keys precede the `"*"` fallback. Fractional codes and
    /// alternate spellings (`"02.500"`) are accepted like Omatrack. Missing or
    /// invalid codes never pick the wildcard; explicit null names mask it.
    /// This does not relabel the recorded driver-ID channel or its stints.
    #[allow(
        clippy::float_cmp,
        reason = "driver codes are exact identifiers, not measurements"
    )]
    pub fn driver_name_for_id(&self, driver_id: f64) -> Option<&str> {
        if !driver_id.is_finite() || driver_id <= 0.0 {
            return None;
        }
        let mappings = field(&self.extra, &["driver", "mappings"])?.as_object()?;
        // Omatrack canonicalizes sorted mapping keys; the last alias wins.
        let exact = mappings.iter().rev().find_map(|(key, value)| {
            let code = key.trim().parse::<f64>().ok()?;
            (code.is_finite() && code > 0.0 && code == driver_id).then_some(value)
        });
        exact
            .or_else(|| mappings.get("*"))?
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_inherit_arrays_replace_and_nulls_mask() {
        let mut base = json!({"car":{"number":"11","class":"LMP2"},"tags":["a"],"event":"race"})
            .as_object()
            .unwrap()
            .clone();
        let overlay = json!({"car":{"number":null},"tags":["b"],"event":"  "});
        merge_metadata(&mut base, overlay.as_object().unwrap());
        assert_eq!(base["car"], json!({"number":null,"class":"LMP2"}));
        assert_eq!(base["tags"], json!(["b"]));
        assert_eq!(base["event"], "race");
    }
}
