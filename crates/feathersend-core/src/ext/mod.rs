//! FeatherSend's own protocol between FeatherSend devices (spec §9): mutual
//! TLS on its own port, pairing, clipboard and resumable transfers.
//!
//! LocalSend's server has no extension points, so this runs as a second
//! HTTPS server next to it, with the same certificate.

pub mod client;
pub mod dto;
pub mod server;
pub mod tls;

pub const API: &str = "/api/feathersend/v1";
pub const MAX_JSON_BYTES: usize = 1024 * 1024;

/// An HTTP status with a short reason, returned by handlers and clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtError {
    pub status: u16,
    pub message: String,
}

impl ExtError {
    pub fn new(status: u16, message: impl Into<String>) -> ExtError {
        ExtError { status, message: message.into() }
    }

    pub fn forbidden() -> ExtError {
        ExtError::new(403, "not paired")
    }

    pub fn bad_request(message: impl Into<String>) -> ExtError {
        ExtError::new(400, message)
    }

    pub fn internal(e: impl std::fmt::Display) -> ExtError {
        ExtError::new(500, e.to_string())
    }
}

impl std::fmt::Display for ExtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.message)
    }
}

impl std::error::Error for ExtError {}
