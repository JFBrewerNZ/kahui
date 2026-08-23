//! Member identity: a single Ed25519 keypair.
//!
//! One keypair does two jobs. Its public half *is* the [`UserId`] that signs
//! events, and the same 32 secret bytes seed the libp2p transport keypair in
//! `kahui-net`. There is no account, no registration and no server to ask: an
//! identity is created offline and is valid the moment it exists.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;

use crate::ids::UserId;

/// Length of a raw Ed25519 secret seed.
pub const SECRET_LEN: usize = 32;
/// Length of a raw Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

/// A signature over an event preimage.
pub type SignatureBytes = [u8; SIGNATURE_LEN];

/// Prefix on a backed-up key, so it is recognisable and hard to confuse with an
/// invite.
pub const KEY_PREFIX: &str = "kahuikey1";

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("secret key must be {SECRET_LEN} bytes, got {0}")]
    BadSecretLength(usize),
    #[error("public key is not a valid Ed25519 point")]
    BadPublicKey,
    #[error("a key starts with `{KEY_PREFIX}`")]
    NotAKey,
    #[error("that key is not valid base58: {0}")]
    BadBase58(#[from] bs58::decode::Error),
}

/// A keypair capable of authoring events.
///
/// Cloneable so the node can hand copies to background tasks; the secret never
/// leaves the process.
#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Creates a brand new identity from the operating system CSPRNG.
    pub fn generate() -> Self {
        let mut seed = [0u8; SECRET_LEN];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        Identity {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// Restores an identity from its 32 secret bytes.
    pub fn from_secret(seed: &[u8]) -> Result<Self, IdentityError> {
        let seed: [u8; SECRET_LEN] = seed
            .try_into()
            .map_err(|_| IdentityError::BadSecretLength(seed.len()))?;
        Ok(Identity {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// The 32 secret bytes. Only ever written to the node's own keyfile.
    pub fn secret_bytes(&self) -> [u8; SECRET_LEN] {
        self.signing.to_bytes()
    }

    /// This identity's public id.
    pub fn user_id(&self) -> UserId {
        UserId::from_bytes(self.signing.verifying_key().to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> SignatureBytes {
        self.signing.sign(message).to_bytes()
    }

    /// The identity as a single line of text, for writing down.
    ///
    /// This *is* the identity, not a hint about where to find it. Whoever holds
    /// it can post as you, in every community you belong to, forever — there is
    /// no server to revoke it at and no password layered on top. It is also the
    /// only way to be the same person on a second machine, or to come back
    /// after losing this one.
    pub fn to_backup_phrase(&self) -> String {
        format!(
            "{KEY_PREFIX}{}",
            bs58::encode(self.secret_bytes()).into_string()
        )
    }

    /// Restores an identity from [`Identity::to_backup_phrase`].
    pub fn from_backup_phrase(text: &str) -> Result<Self, IdentityError> {
        let body = text
            .trim()
            .strip_prefix(KEY_PREFIX)
            .ok_or(IdentityError::NotAKey)?;
        let bytes = bs58::decode(body).into_vec()?;
        Identity::from_secret(&bytes)
    }
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print the secret.
        write!(f, "Identity({})", self.user_id().short())
    }
}

/// Checks `signature` over `message` against `author`.
///
/// Returns `false` rather than erroring for a malformed public key, because a
/// peer sending garbage is an untrusted-input case, not a programming error.
pub fn verify(author: &UserId, message: &[u8], signature: &SignatureBytes) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(author.as_bytes()) else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(signature))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let id = Identity::generate();
        let sig = id.sign(b"kia ora");
        assert!(verify(&id.user_id(), b"kia ora", &sig));
    }

    #[test]
    fn rejects_tampered_message() {
        let id = Identity::generate();
        let sig = id.sign(b"kia ora");
        assert!(!verify(&id.user_id(), b"kia ora!", &sig));
    }

    #[test]
    fn rejects_other_author() {
        let a = Identity::generate();
        let b = Identity::generate();
        let sig = a.sign(b"kia ora");
        assert!(!verify(&b.user_id(), b"kia ora", &sig));
    }

    #[test]
    fn a_backup_phrase_restores_the_same_identity() {
        let original = Identity::generate();
        let phrase = original.to_backup_phrase();
        assert!(phrase.starts_with(KEY_PREFIX));
        let restored = Identity::from_backup_phrase(&phrase).unwrap();
        assert_eq!(restored.user_id(), original.user_id());

        // And it still signs as the same person.
        let signature = restored.sign(b"kia ora");
        assert!(verify(&original.user_id(), b"kia ora", &signature));
    }

    #[test]
    fn text_that_is_not_a_key_is_refused() {
        assert!(matches!(
            Identity::from_backup_phrase("kahui1aaaa"),
            Err(IdentityError::NotAKey)
        ));
        assert!(Identity::from_backup_phrase("kahuikey1!!!!").is_err());
    }

    #[test]
    fn secret_roundtrip_preserves_user_id() {
        let a = Identity::generate();
        let b = Identity::from_secret(&a.secret_bytes()).unwrap();
        assert_eq!(a.user_id(), b.user_id());
    }
}
