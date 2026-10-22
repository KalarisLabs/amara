//! Authenticated encryption using ChaCha20-Poly1305.
//!
//! Used for encrypting task payloads so that relay nodes cannot read them.

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use rand::RngCore;

/// Encrypt a plaintext with ChaCha20-Poly1305 using the given 32-byte key.
///
/// Returns (nonce, ciphertext) where nonce is 12 bytes.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> amara_core::Result<(Vec<u8>, Vec<u8>)> {
    let cipher = ChaCha20Poly1305::new(key.into());

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| amara_core::AmaraError::Encryption(e.to_string()))?;

    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Encrypt with a specific nonce (for deterministic testing).
pub fn encrypt_with_nonce(
    key: &[u8; 32],
    nonce_bytes: &[u8; 12],
    plaintext: &[u8],
) -> amara_core::Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce_bytes);

    cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| amara_core::AmaraError::Encryption(e.to_string()))
}

/// Decrypt a ciphertext with ChaCha20-Poly1305.
pub fn decrypt(
    key: &[u8; 32],
    nonce_bytes: &[u8; 12],
    ciphertext: &[u8],
) -> amara_core::Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| amara_core::AmaraError::Decryption(e.to_string()))
}

/// Derive a symmetric key from a shared secret using HKDF-SHA256.
pub fn derive_key(shared_secret: &[u8], info: &[u8]) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;

    let hk = Hkdf::<Sha256>::new(None, shared_secret);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key)
        .expect("HKDF-SHA256 should never fail for 32-byte output");
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let plaintext = b"hello amara network";

        let (nonce, ciphertext) = encrypt(&key, plaintext).unwrap();
        assert_ne!(ciphertext, plaintext);

        let nonce_arr: [u8; 12] = nonce.try_into().unwrap();
        let decrypted = decrypt(&key, &nonce_arr, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key1 = [42u8; 32];
        let key2 = [99u8; 32];
        let plaintext = b"secret data";

        let (nonce, ciphertext) = encrypt(&key1, plaintext).unwrap();
        let nonce_arr: [u8; 12] = nonce.try_into().unwrap();

        let result = decrypt(&key2, &nonce_arr, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = [42u8; 32];
        let plaintext = b"important data";

        let (nonce, mut ciphertext) = encrypt(&key, plaintext).unwrap();
        let nonce_arr: [u8; 12] = nonce.try_into().unwrap();

        // Tamper with ciphertext
        if let Some(byte) = ciphertext.first_mut() {
            *byte ^= 0xFF;
        }

        let result = decrypt(&key, &nonce_arr, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn key_derivation() {
        let secret = [1u8; 32];
        let key1 = derive_key(&secret, b"session-1");
        let key2 = derive_key(&secret, b"session-2");

        // Different info should produce different keys
        assert_ne!(key1, key2);

        // Same inputs should be deterministic
        let key1b = derive_key(&secret, b"session-1");
        assert_eq!(key1, key1b);
    }

    #[test]
    fn deterministic_encryption() {
        let key = [42u8; 32];
        let nonce = [1u8; 12];
        let plaintext = b"deterministic test";

        let ct1 = encrypt_with_nonce(&key, &nonce, plaintext).unwrap();
        let ct2 = encrypt_with_nonce(&key, &nonce, plaintext).unwrap();
        assert_eq!(ct1, ct2);
    }
}
