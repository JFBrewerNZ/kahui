//! Fixed-width 32-byte identifiers used throughout the protocol.
//!
//! Every identifier in Kahui is 32 bytes. A [`UserId`] is a raw Ed25519 public
//! key; every other id is a BLAKE3-256 digest. They serialise as compact byte
//! strings in binary formats (postcard on the wire, CBOR for sync) and as hex
//! in human-readable formats (JSON), so debugging output stays readable without
//! costing anything on the wire.

use core::fmt;
use core::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Error returned when parsing an identifier from text.
#[derive(Debug, thiserror::Error)]
pub enum IdError {
    #[error("identifier must be 64 hex characters, got {0}")]
    BadLength(usize),
    #[error("identifier is not valid hex: {0}")]
    BadHex(#[from] hex::FromHexError),
}

/// A raw 32-byte value. The building block for every id type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Hash32([u8; 32]);

impl Hash32 {
    /// The all-zero value. Used as a sentinel for a community id that is not
    /// yet known, inside a genesis event that cannot reference its own id.
    pub const ZERO: Self = Hash32([0u8; 32]);

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Hash32(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 32]
    }

    /// BLAKE3-256 of `data`.
    pub fn digest(data: &[u8]) -> Self {
        Hash32(*blake3::hash(data).as_bytes())
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// First 8 hex characters. Enough to eyeball in a terminal, never used for
    /// equality.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..4])
    }

    pub fn from_hex(s: &str) -> Result<Self, IdError> {
        let s = s.trim();
        if s.len() != 64 {
            return Err(IdError::BadLength(s.len()));
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out)?;
        Ok(Hash32(out))
    }

    pub fn try_from_slice(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; 32] = bytes.try_into().ok()?;
        Some(Hash32(arr))
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..", self.short())
    }
}

impl FromStr for Hash32 {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Hash32::from_hex(s)
    }
}

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&self.to_hex())
        } else {
            s.serialize_bytes(&self.0)
        }
    }
}

struct Hash32Visitor;

impl<'de> Visitor<'de> for Hash32Visitor {
    type Value = Hash32;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("32 bytes or a 64-character hex string")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Hash32, E> {
        Hash32::from_hex(v).map_err(de::Error::custom)
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Hash32, E> {
        Hash32::try_from_slice(v).ok_or_else(|| de::Error::invalid_length(v.len(), &self))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Hash32, A::Error> {
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = seq
                .next_element::<u8>()?
                .ok_or_else(|| de::Error::invalid_length(i, &self))?;
        }
        Ok(Hash32(out))
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            d.deserialize_str(Hash32Visitor)
        } else {
            d.deserialize_bytes(Hash32Visitor)
        }
    }
}

/// Declares a distinct id type so the compiler stops us mixing up, say, a
/// channel id and a community id.
macro_rules! declare_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Hash32);

        impl $name {
            pub const ZERO: Self = $name(Hash32::ZERO);

            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                $name(Hash32::from_bytes(bytes))
            }

            pub const fn as_bytes(&self) -> &[u8; 32] {
                self.0.as_bytes()
            }

            pub fn is_zero(&self) -> bool {
                self.0.is_zero()
            }

            pub fn to_hex(self) -> String {
                self.0.to_hex()
            }

            pub fn short(&self) -> String {
                self.0.short()
            }

            pub fn from_hex(s: &str) -> Result<Self, IdError> {
                Hash32::from_hex(s).map($name)
            }

            pub fn try_from_slice(bytes: &[u8]) -> Option<Self> {
                Hash32::try_from_slice(bytes).map($name)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0.short())
            }
        }

        impl FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $name::from_hex(s)
            }
        }
    };
}

declare_id! {
    /// A member identity: their raw Ed25519 public key.
    ///
    /// The same key backs the libp2p transport identity, so a `UserId` maps
    /// deterministically onto a `PeerId` (see the `kahui-net` crate).
    UserId
}

declare_id! {
    /// Content address of a single signed event.
    EventId
}

declare_id! {
    /// A community, what Discord would call a server. Equal to the `EventId`
    /// of its genesis event.
    CommunityId
}

declare_id! {
    /// A channel within a community. Derived from the community id and the
    /// channel name so every member computes the same value.
    ChannelId
}

impl ChannelId {
    /// Channel ids are deterministic: BLAKE3 over a domain tag, the community
    /// id and the lowercased name. Two members who independently create
    /// `#general` produce the same id, so their events merge into one channel
    /// instead of forking it.
    pub fn derive(community: &CommunityId, name: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"kahui.channel.v1\0");
        hasher.update(community.as_bytes());
        hasher.update(name.to_lowercase().as_bytes());
        ChannelId(Hash32::from_bytes(*hasher.finalize().as_bytes()))
    }
}

impl From<EventId> for CommunityId {
    fn from(id: EventId) -> Self {
        CommunityId(id.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let h = Hash32::digest(b"hello");
        assert_eq!(Hash32::from_hex(&h.to_hex()).unwrap(), h);
    }

    #[test]
    fn binary_serde_is_compact_and_roundtrips() {
        let id = EventId(Hash32::digest(b"event"));
        let bytes = postcard::to_stdvec(&id).unwrap();
        // 32 payload bytes plus a one-byte varint length prefix.
        assert_eq!(bytes.len(), 33);
        assert_eq!(postcard::from_bytes::<EventId>(&bytes).unwrap(), id);
    }

    #[test]
    fn human_readable_serde_uses_hex() {
        let id = EventId(Hash32::digest(b"event"));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.to_hex()));
        assert_eq!(serde_json::from_str::<EventId>(&json).unwrap(), id);
    }

    #[test]
    fn channel_ids_are_deterministic_and_case_insensitive() {
        let c = CommunityId(Hash32::digest(b"community"));
        assert_eq!(
            ChannelId::derive(&c, "general"),
            ChannelId::derive(&c, "General")
        );
        assert_ne!(
            ChannelId::derive(&c, "general"),
            ChannelId::derive(&c, "random")
        );
    }
}
