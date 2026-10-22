//! # amara-crypto
//!
//! Cryptographic primitives for the AMARA network:
//! - Node identity (Ed25519 signing + X25519 key agreement)
//! - Packet signing and verification
//! - Authenticated encryption (ChaCha20-Poly1305)
//! - Session key derivation (HKDF)
//! - Stateless end-to-end sealed boxes (ephemeral X25519 + ChaCha20-Poly1305)
//!
//! This crate wraps well-audited libraries. It does not invent cryptography.

pub mod encryption;
pub mod identity;
pub mod sealed;
pub mod session;
pub mod signing;

pub use identity::NodeIdentity;
pub use session::Session;
