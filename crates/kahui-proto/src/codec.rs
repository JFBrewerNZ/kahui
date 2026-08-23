//! Canonical binary encoding.
//!
//! Signatures are only meaningful if every node derives byte-identical
//! preimages from the same event, so the encoding must be canonical: one value,
//! one representation, always.
//!
//! We use [postcard], which gets there by construction — no field names, no
//! map key ordering, varint integers, length-prefixed sequences. The one rule
//! callers must respect is that signed structs contain no `HashMap`/`HashSet`,
//! whose iteration order is not stable. Every signed type in this crate uses
//! `Vec` instead.
//!
//! The encoding is deliberately behind this module. Swapping postcard for a
//! language-neutral canonical format (DAG-CBOR is the obvious candidate, once
//! non-Rust implementations of the protocol exist) is a change to this file and
//! a [`crate::event::PROTOCOL_VERSION`] bump, not a change to every call site.

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("canonical encoding failed: {0}")]
    Encode(postcard::Error),
    #[error("canonical decoding failed: {0}")]
    Decode(postcard::Error),
    #[error("{0} trailing bytes after a complete value")]
    TrailingBytes(usize),
}

/// Encodes a value to its canonical byte representation.
pub fn to_canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    postcard::to_stdvec(value).map_err(CodecError::Encode)
}

/// Decodes a value from canonical bytes.
///
/// Rejects trailing bytes, so a peer cannot pad an event to change its id while
/// keeping it decodable.
pub fn from_canonical<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CodecError> {
    let (value, rest) = postcard::take_from_bytes::<T>(bytes).map_err(CodecError::Decode)?;
    if !rest.is_empty() {
        return Err(CodecError::TrailingBytes(rest.len()));
    }
    Ok(value)
}

/// serde support for 64-byte signatures.
///
/// serde only derives array impls up to length 32, so this is hand-written. It
/// matches the identifier convention: raw bytes in binary formats, hex in
/// human-readable ones.
pub mod signature_serde {
    use super::*;
    use crate::identity::{SignatureBytes, SIGNATURE_LEN};

    pub fn serialize<S: Serializer>(sig: &SignatureBytes, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&hex::encode(sig))
        } else {
            s.serialize_bytes(sig)
        }
    }

    struct SigVisitor;

    impl<'de> Visitor<'de> for SigVisitor {
        type Value = SignatureBytes;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{SIGNATURE_LEN} bytes or a hex string of that length")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let mut out = [0u8; SIGNATURE_LEN];
            hex::decode_to_slice(v, &mut out).map_err(de::Error::custom)?;
            Ok(out)
        }

        fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            v.try_into()
                .map_err(|_| de::Error::invalid_length(v.len(), &self))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = [0u8; SIGNATURE_LEN];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = seq
                    .next_element::<u8>()?
                    .ok_or_else(|| de::Error::invalid_length(i, &self))?;
            }
            Ok(out)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SignatureBytes, D::Error> {
        if d.is_human_readable() {
            d.deserialize_str(SigVisitor)
        } else {
            d.deserialize_bytes(SigVisitor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = to_canonical(&42u32).unwrap();
        bytes.push(0xff);
        assert!(from_canonical::<u32>(&bytes).is_err());
    }

    #[test]
    fn roundtrips() {
        let value = ("hello".to_string(), 7u64, vec![1u8, 2, 3]);
        let bytes = to_canonical(&value).unwrap();
        assert_eq!(
            from_canonical::<(String, u64, Vec<u8>)>(&bytes).unwrap(),
            value
        );
    }
}
