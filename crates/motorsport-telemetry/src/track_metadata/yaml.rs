//! Deserialize under a shared expansion budget, BEFORE materializing YAML
//! aliases. A byte limit on the input alone does not bound the resulting tree.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 65_536;
const MAX_TEXT: usize = 4 * 1_048_576;

struct Budget {
    nodes: usize,
    text: usize,
}

struct Seed<'a> {
    budget: &'a mut Budget,
    depth: usize,
}

pub(super) fn parse(bytes: &[u8]) -> Result<Value, String> {
    let mut documents = serde_yaml::Deserializer::from_slice(bytes);
    let Some(document) = documents.next() else {
        return Ok(Value::Null);
    };
    let mut budget = Budget { nodes: 0, text: 0 };
    let result = Seed {
        budget: &mut budget,
        depth: 0,
    }
    .deserialize(document)
    .map_err(|e| e.to_string())?;
    if documents.next().is_some() {
        return Err("TRACK.yml must contain one YAML document".to_owned());
    }
    Ok(result)
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth > MAX_DEPTH {
            return Err(de::Error::custom("metadata nesting exceeds 64 levels"));
        }
        self.budget.nodes += 1;
        if self.budget.nodes > MAX_NODES {
            return Err(de::Error::custom(
                "expanded metadata exceeds 65536 values/keys",
            ));
        }
        deserializer.deserialize_any(self)
    }
}

impl Seed<'_> {
    fn child(&mut self) -> Seed<'_> {
        Seed {
            budget: self.budget,
            depth: self.depth + 1,
        }
    }

    fn charge_text<E: de::Error>(&mut self, len: usize) -> Result<(), E> {
        self.budget.text = self.budget.text.saturating_add(len);
        if self.budget.text > MAX_TEXT {
            return Err(E::custom("expanded metadata text exceeds 4 MiB"));
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON-compatible metadata without YAML tags")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("metadata numbers must be finite"))
    }
    fn visit_str<E: de::Error>(mut self, value: &str) -> Result<Value, E> {
        self.charge_text(value.len())?;
        Ok(Value::String(value.to_owned()))
    }
    fn visit_string<E: de::Error>(mut self, value: String) -> Result<Value, E> {
        self.charge_text(value.len())?;
        Ok(Value::String(value))
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = access.next_element_seed(self.child())? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = access.next_key_seed(self.child())? {
            let key = match key {
                Value::String(key) => key,
                Value::Number(key) => key.to_string(),
                Value::Bool(key) => key.to_string(),
                _ => {
                    return Err(de::Error::custom(
                        "metadata keys must be scalar strings/numbers/booleans",
                    ))
                }
            };
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate metadata key {key:?}")));
            }
            values.insert(key, access.next_value_seed(self.child())?);
        }
        Ok(Value::Object(values))
    }
}
