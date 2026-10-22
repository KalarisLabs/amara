//! Node identity management.
//!
//! Each AMARA node has an Ed25519 signing key pair and a derived X25519 key pair
//! for Diffie-Hellman key agreement. The PeerId is derived from the public key
//! using BLAKE3.

use amara_core::peer::PeerId;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A node's cryptographic identity.
///
/// Generated once on first run and persisted locally.
/// The signing key is used for packet authentication.
/// The X25519 key (derived from signing key) is used for encryption.
#[derive(Clone)]
pub struct NodeIdentity {
    /// Ed25519 signing key (private).
    signing_key: SigningKey,
    /// Ed25519 verifying key (public).
    verifying_key: VerifyingKey,
    /// Derived PeerId.
    peer_id: PeerId,
    /// Human-readable display name.
    display_name: String,
}

/// Serializable form for persisting identity to disk.
#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    /// Secret key bytes (32 bytes, hex-encoded).
    secret_key_hex: String,
    /// Display name.
    display_name: String,
}

impl NodeIdentity {
    /// Generate a new random identity.
    pub fn generate(display_name: impl Into<String>) -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let peer_id = PeerId::from_public_key(&verifying_key.to_bytes());

        Self {
            signing_key,
            verifying_key,
            peer_id,
            display_name: display_name.into(),
        }
    }

    /// Generate identity using a specific RNG (for deterministic testing).
    pub fn generate_deterministic(
        display_name: impl Into<String>,
        rng: &mut (impl rand::RngCore + rand::CryptoRng),
    ) -> Self {
        let signing_key = SigningKey::generate(rng);
        let verifying_key = signing_key.verifying_key();
        let peer_id = PeerId::from_public_key(&verifying_key.to_bytes());

        Self {
            signing_key,
            verifying_key,
            peer_id,
            display_name: display_name.into(),
        }
    }

    /// Load identity from a file, or generate and save if it doesn't exist.
    pub fn load_or_generate(path: &Path, display_name: &str) -> amara_core::Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let identity = Self::generate(display_name);
            identity.save(path)?;
            Ok(identity)
        }
    }

    /// Load identity from a JSON file.
    pub fn load(path: &Path) -> amara_core::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let stored: StoredIdentity = serde_json::from_str(&data)
            .map_err(|e| amara_core::AmaraError::Config(format!("invalid identity file: {e}")))?;

        let secret_bytes = hex::decode(&stored.secret_key_hex)
            .map_err(|e| amara_core::AmaraError::Config(format!("invalid key hex: {e}")))?;

        if secret_bytes.len() != 32 {
            return Err(amara_core::AmaraError::Config(
                "invalid key length".to_string(),
            ));
        }

        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&secret_bytes);

        let signing_key = SigningKey::from_bytes(&key_bytes);
        let verifying_key = signing_key.verifying_key();
        let peer_id = PeerId::from_public_key(&verifying_key.to_bytes());

        Ok(Self {
            signing_key,
            verifying_key,
            peer_id,
            display_name: stored.display_name,
        })
    }

    /// Save identity to a JSON file.
    pub fn save(&self, path: &Path) -> amara_core::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let stored = StoredIdentity {
            secret_key_hex: hex::encode(self.signing_key.to_bytes()),
            display_name: self.display_name.clone(),
        };

        let json = serde_json::to_string_pretty(&stored)
            .map_err(|e| amara_core::AmaraError::Serialization(e.to_string()))?;

        std::fs::write(path, json)?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    /// Get the PeerId.
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Get the display name.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Set the display name.
    pub fn set_display_name(&mut self, name: impl Into<String>) {
        self.display_name = name.into();
    }

    /// Get the signing key reference.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Get the verifying (public) key.
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Get the public key bytes (32 bytes).
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Derive an X25519 static secret for key agreement.
    /// We derive it from the Ed25519 secret key using BLAKE3 as a KDF.
    pub fn x25519_secret(&self) -> x25519_dalek::StaticSecret {
        let hash = blake3::derive_key("amara-x25519-derivation", &self.signing_key.to_bytes());
        x25519_dalek::StaticSecret::from(hash)
    }

    /// Derive the X25519 public key.
    pub fn x25519_public(&self) -> x25519_dalek::PublicKey {
        x25519_dalek::PublicKey::from(&self.x25519_secret())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_generation() {
        let id = NodeIdentity::generate("test-node");
        assert_eq!(id.display_name(), "test-node");
        assert!(!id.peer_id().is_broadcast());
    }

    #[test]
    fn deterministic_generation() {
        use rand::SeedableRng;
        let mut rng1 = rand_chacha::ChaCha20Rng::seed_from_u64(42);
        let mut rng2 = rand_chacha::ChaCha20Rng::seed_from_u64(42);

        let id1 = NodeIdentity::generate_deterministic("node", &mut rng1);
        let id2 = NodeIdentity::generate_deterministic("node", &mut rng2);

        assert_eq!(id1.peer_id(), id2.peer_id());
        assert_eq!(id1.public_key_bytes(), id2.public_key_bytes());
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");

        let original = NodeIdentity::generate("save-test");
        original.save(&path).unwrap();

        let loaded = NodeIdentity::load(&path).unwrap();
        assert_eq!(original.peer_id(), loaded.peer_id());
        assert_eq!(original.display_name(), loaded.display_name());
        assert_eq!(original.public_key_bytes(), loaded.public_key_bytes());
    }

    #[test]
    fn x25519_key_derivation() {
        let id1 = NodeIdentity::generate("alice");
        let id2 = NodeIdentity::generate("bob");

        let shared1 = id1.x25519_secret().diffie_hellman(&id2.x25519_public());
        let shared2 = id2.x25519_secret().diffie_hellman(&id1.x25519_public());

        assert_eq!(shared1.as_bytes(), shared2.as_bytes());
    }
}
