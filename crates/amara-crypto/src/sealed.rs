//! Stateless end-to-end encryption ("sealed boxes") for mesh payloads.
//!
//! Every message gets a fresh ephemeral X25519 key, so there is no session
//! state to synchronise over a lossy, multi-hop mesh and compromise of the
//! sender's long-term key never exposes past messages it sent.
//!
//! ```text
//! sealed = ephemeral_public (32) || ChaCha20-Poly1305(key, nonce = 0, plaintext, aad)
//! key    = HKDF-SHA256(ikm = X25519(ephemeral, recipient),
//!                      info = "amara-seal-v1" || ephemeral_public || recipient_public)
//! ```
//!
//! The key is unique per message, so the all-zero nonce is never reused.
//! Callers pass the packet's routing metadata as `aad`, binding the ciphertext
//! to one packet so a relay cannot splice it into another.

use amara_core::{AmaraError, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::rngs::OsRng;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

/// Bytes added to every sealed message (ephemeral key + Poly1305 tag).
pub const SEAL_OVERHEAD: usize = 32 + 16;

fn seal_key(shared: &[u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> [u8; 32] {
    let mut info = Vec::with_capacity(13 + 64);
    info.extend_from_slice(b"amara-seal-v1");
    info.extend_from_slice(ephemeral);
    info.extend_from_slice(recipient);
    crate::encryption::derive_key(shared, &info)
}

/// Encrypt `plaintext` so only the holder of `recipient`'s X25519 secret can read it.
pub fn seal(recipient: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let recipient_pk = PublicKey::from(*recipient);
    let ephemeral = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral_pk = PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&recipient_pk);
    if !shared.was_contributory() {
        return Err(AmaraError::Encryption(
            "non-contributory recipient key".into(),
        ));
    }

    let key = seal_key(shared.as_bytes(), &ephemeral_pk, recipient);
    let cipher = ChaCha20Poly1305::new(&key.into());
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&[0u8; 12]),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| AmaraError::Encryption(e.to_string()))?;

    let mut out = Vec::with_capacity(32 + ciphertext.len());
    out.extend_from_slice(&ephemeral_pk);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a sealed message with our X25519 secret.
pub fn open(secret: &StaticSecret, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < SEAL_OVERHEAD {
        return Err(AmaraError::Decryption("sealed payload too short".into()));
    }
    let mut ephemeral_pk = [0u8; 32];
    ephemeral_pk.copy_from_slice(&sealed[..32]);
    let recipient_pk = PublicKey::from(secret).to_bytes();

    let shared = secret.diffie_hellman(&PublicKey::from(ephemeral_pk));
    if !shared.was_contributory() {
        return Err(AmaraError::Decryption(
            "non-contributory ephemeral key".into(),
        ));
    }

    let key = seal_key(shared.as_bytes(), &ephemeral_pk, &recipient_pk);
    let cipher = ChaCha20Poly1305::new(&key.into());
    cipher
        .decrypt(
            Nonce::from_slice(&[0u8; 12]),
            Payload {
                msg: &sealed[32..],
                aad,
            },
        )
        .map_err(|_| AmaraError::Decryption("authentication failed".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    #[test]
    fn seal_open_roundtrip() {
        let bob = NodeIdentity::generate("bob");
        let sealed = seal(&bob.x25519_public().to_bytes(), b"hi bob", b"hdr").unwrap();
        assert_eq!(sealed.len(), 6 + SEAL_OVERHEAD);
        assert_eq!(
            open(&bob.x25519_secret(), &sealed, b"hdr").unwrap(),
            b"hi bob"
        );
    }

    #[test]
    fn wrong_recipient_or_aad_fails() {
        let bob = NodeIdentity::generate("bob");
        let eve = NodeIdentity::generate("eve");
        let sealed = seal(&bob.x25519_public().to_bytes(), b"secret", b"hdr").unwrap();
        assert!(open(&eve.x25519_secret(), &sealed, b"hdr").is_err());
        assert!(open(&bob.x25519_secret(), &sealed, b"other").is_err());
    }

    #[test]
    fn each_seal_is_unique() {
        let bob = NodeIdentity::generate("bob");
        let pk = bob.x25519_public().to_bytes();
        assert_ne!(
            seal(&pk, b"same", b"").unwrap(),
            seal(&pk, b"same", b"").unwrap()
        );
    }
}
