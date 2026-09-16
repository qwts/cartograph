//! Allocation-free value traversal before v2 typed decoding. The complete SQL
//! text is already byte-bounded; container/depth/string budgets are checked while
//! streaming, and duplicate object keys fail rather than disappearing in Value.
use super::StagingError;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use std::{collections::BTreeSet, fmt};

pub(super) fn preflight(json: &str) -> Result<(), StagingError> {
    let mut decoder = serde_json::Deserializer::from_str(json);
    let mut remaining = 4096usize;
    Seed {
        depth: 0,
        remaining: &mut remaining,
        array_limit: 65,
    }
    .deserialize(&mut decoder)
    .map_err(|_| StagingError::InvalidRecord)?;
    decoder.end().map_err(|_| StagingError::InvalidRecord)
}
struct Seed<'a> {
    depth: usize,
    remaining: &'a mut usize,
    array_limit: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        if self.depth > 16 || *self.remaining == 0 {
            return Err(D::Error::custom("shape bound"));
        }
        *self.remaining -= 1;
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded staged metadata")
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
        if value.len() > super::MAX_STAGED_RECORD_BYTES {
            Err(E::custom("string bound"))
        } else {
            Ok(())
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let mut count = 0;
        // next_element_seed inspects one value before discovering whether a
        // sequence ends; the extra element still has all recursive budgets.
        while a
            .next_element_seed(Seed {
                depth: self.depth + 1,
                remaining: self.remaining,
                array_limit: 65,
            })?
            .is_some()
        {
            count += 1;
            if count > self.array_limit {
                return Err(A::Error::custom("array bound"));
            }
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = a.next_key::<String>()? {
            if key.len() > 64 || keys.len() == 32 || !keys.insert(key.clone()) {
                return Err(A::Error::custom("object bound"));
            }
            let array_limit = match key.as_str() {
                "evidence" | "citations" | "source_evidence_ids" | "evidence_ids" => 12,
                "candidates" => 8,
                "omissions" => 64,
                _ => 65,
            };
            a.next_value_seed(Seed {
                depth: self.depth + 1,
                remaining: self.remaining,
                array_limit,
            })?;
        }
        Ok(())
    }
}
