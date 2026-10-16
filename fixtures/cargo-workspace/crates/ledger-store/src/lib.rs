//! Account storage.

use std::collections::HashMap;

use ledger_core::{AccountId, Money};

/// Stores account balances.
pub trait AccountStore {
    /// The balance of an account; zero if unknown.
    fn balance(&self, account: &AccountId) -> Money;
    /// Adds `amount` (may be negative) to an account.
    fn post(&mut self, account: &AccountId, amount: Money);
}

/// An in-memory store for tests and demos.
#[derive(Debug, Default)]
pub struct MemoryStore {
    balances: HashMap<AccountId, Money>,
}

impl AccountStore for MemoryStore {
    fn balance(&self, account: &AccountId) -> Money {
        self.balances.get(account).copied().unwrap_or_default()
    }

    fn post(&mut self, account: &AccountId, amount: Money) {
        let entry = self.balances.entry(account.clone()).or_default();
        *entry = *entry + amount;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts_accumulate() {
        let mut store = MemoryStore::default();
        let acct = AccountId("a".into());
        store.post(&acct, Money(100));
        store.post(&acct, Money(-30));
        assert_eq!(store.balance(&acct), Money(70));
    }
}
