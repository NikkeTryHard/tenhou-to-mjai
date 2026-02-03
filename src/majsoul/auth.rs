//! Authentication utilities for Majsoul CN server
//!
//! CN server uses native login with HMAC-SHA256 password hashing.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Hash password using HMAC-SHA256 with key "lailai" (Majsoul CN auth)
pub fn hash_password(password: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(b"lailai").expect("HMAC can take key of any size");
    mac.update(password.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_password_length() {
        let result = hash_password("testpass");
        assert_eq!(result.len(), 64); // SHA256 hex = 64 chars
    }

    #[test]
    fn test_hash_password_deterministic() {
        assert_eq!(hash_password("test"), hash_password("test"));
    }

    #[test]
    fn test_hash_password_known_value() {
        // Verified against Python: hmac.new(b"lailai", b"test", hashlib.sha256).hexdigest()
        let result = hash_password("test");
        assert_eq!(
            result,
            "8e4aa650187e80b9272050ef48a97e61a5a8a1efe994cc0d68eb9fb873785638"
        );
    }

    #[test]
    fn test_hash_password_empty() {
        let result = hash_password("");
        assert_eq!(result.len(), 64);
        // Verified: hmac.new(b"lailai", b"", hashlib.sha256).hexdigest()
        assert_eq!(
            result,
            "2eb26f7c93cda48f98fe730fbd00e4b5603935461b1334bc07b87eb94894461e"
        );
    }
}
