//! Sessions for shop customers.

/// Whether `token` looks like a session token: 32 hex characters.
pub fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert!(valid_token(&"a".repeat(32)));
        assert!(!valid_token("short"));
    }
}
