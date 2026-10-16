//! Money and account types shared by every ledger crate.

use std::fmt;
use std::ops::{Add, Sub};

/// An amount in minor units (cents).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(pub i64);

impl Money {
    /// Zero.
    pub const ZERO: Money = Money(0);

    /// Applies a percentage, rounding half away from zero.
    pub fn percent(self, pct: i64) -> Money {
        let raw = self.0 * pct;
        Money((raw + raw.signum() * 50) / 100)
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}

impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, (self.0 % 100).abs())
    }
}

/// Identifies an account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_rounds_half_away_from_zero() {
        assert_eq!(Money(1050).percent(10), Money(105));
        assert_eq!(Money(5).percent(10), Money(1));
    }

    #[test]
    fn display() {
        assert_eq!(Money(12345).to_string(), "123.45");
    }
}
