//! API tokens and scopes.

use ledger_core::AccountId;

/// What a token may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Read balances and invoices.
    Read,
    /// Create invoices.
    Bill,
}

/// An API token bound to one account.
#[derive(Debug, Clone)]
pub struct Token {
    /// The account the token acts for.
    pub account: AccountId,
    /// Granted scopes.
    pub scopes: Vec<Scope>,
}

/// Checks that `token` grants `scope`.
pub fn authorize(token: &Token, scope: Scope) -> Result<&AccountId, String> {
    if token.scopes.contains(&scope) {
        Ok(&token.account)
    } else {
        Err(format!("token lacks scope {scope:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_required() {
        let token = Token { account: AccountId("a".into()), scopes: vec![Scope::Read] };
        assert!(authorize(&token, Scope::Read).is_ok());
        assert!(authorize(&token, Scope::Bill).is_err());
    }
}
