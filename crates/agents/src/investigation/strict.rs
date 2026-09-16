//! Streaming shape admission before typed allocations. Byte length is checked
//! first; duplicate object keys and trailing JSON never disappear in a Value.
use super::InvestigationError;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use std::{collections::BTreeSet, fmt};

pub(super) fn preflight(
    json: &str,
    bytes: usize,
    depth: usize,
    values: usize,
    array_items: usize,
) -> Result<(), InvestigationError> {
    if json.len() > bytes {
        return Err(InvestigationError::LimitExceeded);
    }
    let mut decoder = serde_json::Deserializer::from_str(json);
    let mut remaining = values;
    Seed {
        depth: 0,
        max_depth: depth,
        remaining: &mut remaining,
        array_items,
        string_bytes: bytes,
    }
    .deserialize(&mut decoder)
    .map_err(|_| InvestigationError::InvalidInput)?;
    decoder.end().map_err(|_| InvestigationError::InvalidInput)
}

struct Seed<'a> {
    depth: usize,
    max_depth: usize,
    remaining: &'a mut usize,
    array_items: usize,
    string_bytes: usize,
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        if self.depth > self.max_depth || *self.remaining == 0 {
            return Err(D::Error::custom("investigation shape bound"));
        }
        *self.remaining -= 1;
        d.deserialize_any(self)
    }
}

impl Seed<'_> {
    fn child(&mut self) -> Seed<'_> {
        Seed {
            depth: self.depth + 1,
            max_depth: self.max_depth,
            remaining: self.remaining,
            array_items: self.array_items,
            string_bytes: self.string_bytes,
        }
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded investigation data")
    }
    fn visit_bool<E: Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: Error>(self, value: &str) -> Result<(), E> {
        if value.len() > self.string_bytes {
            Err(E::custom("investigation string bound"))
        } else {
            Ok(())
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut a: A) -> Result<(), A::Error> {
        let mut count = 0;
        while a.next_element_seed(self.child())?.is_some() {
            count += 1;
            if count > self.array_items {
                return Err(A::Error::custom("array bound"));
            }
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut a: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = a.next_key::<String>()? {
            if key.len() > 128 || keys.len() >= 64 || !keys.insert(key) {
                return Err(A::Error::custom("object bound"));
            }
            a.next_value_seed(self.child())?;
        }
        Ok(())
    }
}
