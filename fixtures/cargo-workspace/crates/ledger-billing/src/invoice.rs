//! Invoices.

use ledger_core::{AccountId, Money};
use ledger_store::AccountStore;

use crate::discount::discount_on;
use crate::tax::tax_on;

/// One billed item.
#[derive(Debug, Clone)]
pub struct LineItem {
    /// What was sold.
    pub description: String,
    /// Net price.
    pub net: Money,
}

/// A computed invoice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoice {
    /// Sum of line items.
    pub net: Money,
    /// Volume discount.
    pub discount: Money,
    /// Tax on the discounted amount.
    pub tax: Money,
    /// Amount charged.
    pub total: Money,
}

/// Computes an invoice and charges the account.
pub fn bill(store: &mut impl AccountStore, account: &AccountId, items: &[LineItem]) -> Invoice {
    let net = items.iter().fold(Money::ZERO, |sum, item| sum + item.net);
    let discount = discount_on(net);
    let tax = tax_on(net - discount);
    let total = net - discount + tax;
    store.post(account, Money(-total.0));
    Invoice { net, discount, tax, total }
}

#[cfg(test)]
mod tests {
    use ledger_store::MemoryStore;

    use super::*;

    #[test]
    fn bills_and_charges() {
        let mut store = MemoryStore::default();
        let acct = AccountId("a".into());
        let items = [LineItem { description: "plan".into(), net: Money(1000) }];
        let invoice = bill(&mut store, &acct, &items);
        assert_eq!(invoice.total, Money(1200));
        assert_eq!(store.balance(&acct), Money(-1200));
    }
}
