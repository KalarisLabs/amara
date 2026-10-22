//! Session management for encrypted peer-to-peer communication.
//!
//! A session represents an established encrypted channel between two peers
//! after key exchange. Sessions derive symmetric keys from the X25519
//! shared secret.

use crate::encryption;
use amara_core::peer::PeerId;

/// An encrypted session between two peers.
#[derive(Clone)]
pub struct Session {
    /// Local peer ID.
    pub local_peer: PeerId,
    /// Remote peer ID.
    pub remote_peer: PeerId,
    /// Session ID (8 bytes, derived from shared secret).
    pub session_id: [u8; 8],
    /// Symmetric encryption key (derived via HKDF).
    send_key: [u8; 32],
    /// Symmetric decryption key (derived via HKDF).
    recv_key: [u8; 32],
    /// Monotonic nonce counter for send direction.
    send_counter: u64,
}

impl Session {
    /// Create a new session from a shared secret.
    ///
    /// The `is_initiator` flag determines key direction to prevent
    /// both sides using the same key for send/recv.
    pub fn from_shared_secret(
        local_peer: PeerId,
        remote_peer: PeerId,
        shared_secret: &[u8; 32],
        is_initiator: bool,
    ) -> Self {
        // Derive session ID
        let session_hash = blake3::derive_key("amara-session-id", shared_secret);
        let mut session_id = [0u8; 8];
        session_id.copy_from_slice(&session_hash[..8]);

        // Derive directional keys
        let key_a = encryption::derive_key(shared_secret, b"amara-session-key-initiator");
        let key_b = encryption::derive_key(shared_secret, b"amara-session-key-responder");

        let (send_key, recv_key) = if is_initiator {
            (key_a, key_b)
        } else {
            (key_b, key_a)
        };

        Self {
            local_peer,
            remote_peer,
            session_id,
            send_key,
            recv_key,
            send_counter: 0,
        }
    }

    /// Encrypt a message for the remote peer.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> amara_core::Result<(Vec<u8>, Vec<u8>)> {
        // Build nonce from counter (prevents reuse)
        let mut nonce = [0u8; 12];
        nonce[4..12].copy_from_slice(&self.send_counter.to_be_bytes());
        self.send_counter += 1;

        let ciphertext = encryption::encrypt_with_nonce(&self.send_key, &nonce, plaintext)?;
        Ok((nonce.to_vec(), ciphertext))
    }

    /// Decrypt a message from the remote peer.
    pub fn decrypt(
        &self,
        nonce_bytes: &[u8; 12],
        ciphertext: &[u8],
    ) -> amara_core::Result<Vec<u8>> {
        encryption::decrypt(&self.recv_key, nonce_bytes, ciphertext)
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("local_peer", &self.local_peer)
            .field("remote_peer", &self.remote_peer)
            .field("session_id", &hex::encode(self.session_id))
            .field("send_counter", &self.send_counter)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    #[test]
    fn session_encrypt_decrypt() {
        let alice = NodeIdentity::generate("alice");
        let bob = NodeIdentity::generate("bob");

        // Simulate DH key exchange
        let shared_alice = alice.x25519_secret().diffie_hellman(&bob.x25519_public());
        let shared_bob = bob.x25519_secret().diffie_hellman(&alice.x25519_public());

        assert_eq!(shared_alice.as_bytes(), shared_bob.as_bytes());

        let mut session_alice = Session::from_shared_secret(
            alice.peer_id(),
            bob.peer_id(),
            shared_alice.as_bytes(),
            true,
        );
        let session_bob = Session::from_shared_secret(
            bob.peer_id(),
            alice.peer_id(),
            shared_bob.as_bytes(),
            false,
        );

        // Session IDs should match
        assert_eq!(session_alice.session_id, session_bob.session_id);

        // Encrypt from Alice, decrypt at Bob
        let plaintext = b"hello from alice";
        let (nonce, ciphertext) = session_alice.encrypt(plaintext).unwrap();
        let nonce_arr: [u8; 12] = nonce.try_into().unwrap();
        let decrypted = session_bob.decrypt(&nonce_arr, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn nonce_increments() {
        let id = NodeIdentity::generate("test");
        let shared = [42u8; 32];

        let mut session =
            Session::from_shared_secret(id.peer_id(), PeerId::from_bytes([1; 8]), &shared, true);

        let (nonce1, _) = session.encrypt(b"msg1").unwrap();
        let (nonce2, _) = session.encrypt(b"msg2").unwrap();

        assert_ne!(nonce1, nonce2, "Nonces must be unique");
    }
}
