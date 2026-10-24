//! Serde for [`ModifiedKey`] and [`KeyCombo`].
//!
//! Hand-written rather than derived, because a combo is three different TOML
//! shapes and `#[serde(untagged)]` would report all of them as "data did not
//! match any variant" when any one of them has a typo. These visitors report
//! the actual problem instead -- which key, which modifier, and what was
//! expected.

use std::fmt;

use serde::{
    de::{self, SeqAccess, Visitor},
    ser::SerializeSeq,
    Deserialize, Deserializer, Serialize, Serializer,
};

use super::combo::{Key, KeyCombo, ModifiedKey};

/// The `{ key = "d", mod = ["Ctrl"] }` form.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyMapForm {
    key: String,
    #[serde(default, alias = "mods", alias = "modifiers")]
    r#mod: Vec<String>,
}

impl<'de> Deserialize<'de> for ModifiedKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = ModifiedKey;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#"a key such as "j" or "ctrl+d", or { key = "d", mod = ["Ctrl"] }"#)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                v.parse().map_err(de::Error::custom)
            }

            fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let form = KeyMapForm::deserialize(de::value::MapAccessDeserializer::new(map))?;

                // Reuse the string parser for the key itself, then fold the
                // explicit modifier list on top, so "ctrl+d" and
                // { key = "d", mod = ["Ctrl"] } cannot drift apart.
                let mut parsed: ModifiedKey = form.key.parse().map_err(de::Error::custom)?;
                for name in &form.r#mod {
                    parsed.mods.set(name).map_err(de::Error::custom)?;
                }

                // An uppercase key already implies Shift; see
                // ModifiedKey::from_event.
                if matches!(parsed.key, Key::Char(c) if c.is_uppercase()) {
                    parsed.mods.shift = false;
                }

                Ok(parsed)
            }
        }

        deserializer.deserialize_any(V)
    }
}

impl Serialize for ModifiedKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for KeyCombo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = KeyCombo;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    r#"a key ("j"), a modified key ({ key = "d", mod = ["Ctrl"] }), or a chord (["g", "g"])"#,
                )
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(KeyCombo::single(v.parse().map_err(de::Error::custom)?))
            }

            fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let key = ModifiedKey::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(KeyCombo::single(key))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = seq.next_element::<ModifiedKey>()? {
                    keys.push(key);
                }
                if keys.is_empty() {
                    return Err(de::Error::custom("a key combo cannot be empty"));
                }
                Ok(KeyCombo(keys))
            }
        }

        deserializer.deserialize_any(V)
    }
}

impl Serialize for KeyCombo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // A single key round-trips as a bare string, which is the form almost
        // every binding is written in.
        if let [only] = self.0.as_slice() {
            return serializer.serialize_str(&only.to_string());
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for key in &self.0 {
            seq.serialize_element(key)?;
        }
        seq.end()
    }
}
