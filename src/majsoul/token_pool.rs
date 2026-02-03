use anyhow::{Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Represents a single authenticated Majsoul account token.
#[derive(Debug, Clone)]
pub struct AccountToken {
    pub uid: u64,
    pub token: String,
    pub server: String,
}

/// A thread-safe pool of account tokens with round-robin selection.
#[derive(Debug)]
pub struct TokenPool {
    tokens: Vec<AccountToken>,
    index: AtomicUsize,
}

impl TokenPool {
    /// Create a new token pool from a vector of tokens.
    pub fn new(tokens: Vec<AccountToken>) -> Self {
        Self {
            tokens,
            index: AtomicUsize::new(0),
        }
    }

    /// Load tokens from a file.
    ///
    /// File format: one token per line as `uid,token,server`
    /// Lines starting with `#` are treated as comments.
    /// Empty lines are ignored.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read token file: {:?}", path.as_ref()))?;
        Self::parse_tokens(&content)
    }

    /// Parse tokens from a string.
    ///
    /// Format: one token per line as `uid,token,server`
    /// Lines starting with `#` are treated as comments.
    /// Empty lines are ignored.
    pub fn parse_tokens(content: &str) -> Result<Self> {
        let mut tokens = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() != 3 {
                anyhow::bail!(
                    "Invalid token format at line {}: expected 'uid,token,server', got '{}'",
                    line_num + 1,
                    line
                );
            }

            let uid: u64 = parts[0].trim().parse().with_context(|| {
                format!(
                    "Invalid uid at line {}: '{}' is not a valid u64",
                    line_num + 1,
                    parts[0].trim()
                )
            })?;

            let token = parts[1].trim().to_string();
            let server = parts[2].trim().to_string();

            tokens.push(AccountToken { uid, token, server });
        }

        Ok(Self::new(tokens))
    }

    /// Get the next token using round-robin selection.
    ///
    /// # Panics
    /// Panics if the pool is empty.
    pub fn next(&self) -> AccountToken {
        assert!(!self.tokens.is_empty(), "TokenPool is empty");

        let idx = self.index.fetch_add(1, Ordering::Relaxed) % self.tokens.len();
        self.tokens[idx].clone()
    }

    /// Get the next token using round-robin selection, or None if pool is empty.
    ///
    /// This is a safe alternative to `next()` that doesn't panic on empty pools.
    pub fn try_next(&self) -> Option<AccountToken> {
        if self.tokens.is_empty() {
            return None;
        }

        let idx = self.index.fetch_add(1, Ordering::Relaxed) % self.tokens.len();
        Some(self.tokens[idx].clone())
    }

    /// Returns the number of tokens in the pool.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Returns true if the pool contains no tokens.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_pool_round_robin() {
        let tokens = vec![
            AccountToken {
                uid: 1,
                token: "token1".to_string(),
                server: "en".to_string(),
            },
            AccountToken {
                uid: 2,
                token: "token2".to_string(),
                server: "jp".to_string(),
            },
            AccountToken {
                uid: 3,
                token: "token3".to_string(),
                server: "en".to_string(),
            },
        ];

        let pool = TokenPool::new(tokens);

        // First round
        assert_eq!(pool.next().uid, 1);
        assert_eq!(pool.next().uid, 2);
        assert_eq!(pool.next().uid, 3);

        // Should wrap around
        assert_eq!(pool.next().uid, 1);
        assert_eq!(pool.next().uid, 2);
        assert_eq!(pool.next().uid, 3);

        // And continue wrapping
        assert_eq!(pool.next().uid, 1);
    }

    #[test]
    fn test_token_pool_from_file() {
        let content = r#"
# This is a comment
12345,abc123token,en
67890,def456token,jp

# Another comment
11111,ghi789token,en
"#;

        let pool = TokenPool::parse_tokens(content).unwrap();

        assert_eq!(pool.len(), 3);
        assert!(!pool.is_empty());

        let t1 = pool.next();
        assert_eq!(t1.uid, 12345);
        assert_eq!(t1.token, "abc123token");
        assert_eq!(t1.server, "en");

        let t2 = pool.next();
        assert_eq!(t2.uid, 67890);
        assert_eq!(t2.token, "def456token");
        assert_eq!(t2.server, "jp");

        let t3 = pool.next();
        assert_eq!(t3.uid, 11111);
        assert_eq!(t3.token, "ghi789token");
        assert_eq!(t3.server, "en");
    }

    #[test]
    fn test_token_pool_empty() {
        let pool = TokenPool::new(vec![]);
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_token_pool_parse_error_invalid_format() {
        let content = "12345,token_only";
        let result = TokenPool::parse_tokens(content);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid token format"));
    }

    #[test]
    fn test_token_pool_parse_error_invalid_uid() {
        let content = "not_a_number,token,en";
        let result = TokenPool::parse_tokens(content);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid uid"));
    }

    #[test]
    fn test_token_pool_try_next() {
        // Test with non-empty pool
        let tokens = vec![
            AccountToken {
                uid: 1,
                token: "token1".to_string(),
                server: "en".to_string(),
            },
            AccountToken {
                uid: 2,
                token: "token2".to_string(),
                server: "jp".to_string(),
            },
        ];

        let pool = TokenPool::new(tokens);
        assert_eq!(pool.try_next().unwrap().uid, 1);
        assert_eq!(pool.try_next().unwrap().uid, 2);
        assert_eq!(pool.try_next().unwrap().uid, 1); // wraps around

        // Test with empty pool - should return None instead of panicking
        let empty_pool = TokenPool::new(vec![]);
        assert!(empty_pool.try_next().is_none());
    }
}
