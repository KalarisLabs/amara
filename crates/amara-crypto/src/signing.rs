//! Packet signing and verification.
//!
//! All outgoing AMARA packets are signed with the sender's Ed25519 key.
//! The signature covers the entire packet except:
//! - TTL byte (offset 3) — allows relay nodes to decrement
//! - The signature itself (appended after payload)

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Sign a packet (header + payload), producing a 64-byte Ed25519 signature.
///
/// The TTL byte at offset 3 is zeroed for signing purposes, allowing relays
/// to decrement TTL without invalidating the signature.
pub fn sign_packet(signing_key: &SigningKey, header: &[u8], payload: &[u8]) -> [u8; 64] {
    let mut signable = Vec::with_capacity(header.len() + payload.len());
    signable.extend_from_slice(header);
    signable.extend_from_slice(payload);

    // Zero the TTL byte for signing
    if signable.len() > 3 {
        signable[3] = 0;
    }

    let signature = signing_key.sign(&signable);
    signature.to_bytes()
}

/// Verify a packet signature.
///
/// Returns true if the signature is valid for the given header + payload,
/// with TTL zeroed for verification.
pub fn verify_packet(
    verifying_key: &VerifyingKey,
    header: &[u8],
    payload: &[u8],
    signature_bytes: &[u8; 64],
) -> bool {
    let mut verifiable = Vec::with_capacity(header.len() + payload.len());
    verifiable.extend_from_slice(header);
    verifiable.extend_from_slice(payload);

    // Zero the TTL byte for verification
    if verifiable.len() > 3 {
        verifiable[3] = 0;
    }

    let Ok(signature) = Signature::from_bytes(signature_bytes) else {
        return false;
    };

    verifying_key.verify(&verifiable, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn sign_and_verify() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let header = [0u8; 42];
        let payload = b"hello amara";

        let sig = sign_packet(&signing_key, &header, payload);
        assert!(verify_packet(&verifying_key, &header, payload, &sig));
    }

    #[test]
    fn ttl_change_does_not_invalidate() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let mut header = [0u8; 42];
        header[3] = 7; // TTL = 7
        let payload = b"test payload";

        let sig = sign_packet(&signing_key, &header, payload);

        // Change TTL (simulating relay)
        header[3] = 5;

        // Signature should still be valid
        assert!(verify_packet(&verifying_key, &header, payload, &sig));
    }

    #[test]
    fn tampered_payload_fails() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let header = [0u8; 42];
        let payload = b"original";

        let sig = sign_packet(&signing_key, &header, payload);

        let tampered = b"tampered";
        assert!(!verify_packet(&verifying_key, &header, tampered, &sig));
    }

    #[test]
    fn wrong_key_fails() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let wrong_key = SigningKey::generate(&mut OsRng);

        let header = [0u8; 42];
        let payload = b"test";

        let sig = sign_packet(&signing_key, &header, payload);
        assert!(!verify_packet(
            &wrong_key.verifying_key(),
            &header,
            payload,
            &sig
        ));
    }
}
