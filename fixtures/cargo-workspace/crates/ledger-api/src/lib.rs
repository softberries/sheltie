//! Request handlers.

use ledger_auth::{Scope, Token, authorize};
use ledger_billing::{Invoice, LineItem, bill};
use ledger_store::AccountStore;

/// `POST /invoices`: bills the token's account.
pub fn create_invoice(
    store: &mut impl AccountStore,
    token: &Token,
    items: &[LineItem],
) -> Result<Invoice, String> {
    let account = authorize(token, Scope::Bill)?;
    Ok(bill(store, account, items))
}

#[cfg(test)]
mod tests {
    use ledger_core::{AccountId, Money};
    use ledger_store::MemoryStore;

    use super::*;

    #[test]
    fn requires_bill_scope() {
        let mut store = MemoryStore::default();
        let token = Token { account: AccountId("a".into()), scopes: vec![Scope::Read] };
        let items = [LineItem { description: "x".into(), net: Money(1) }];
        assert!(create_invoice(&mut store, &token, &items).is_err());
    }
}
