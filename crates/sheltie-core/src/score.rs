//! Scores in the closed unit interval.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Error returned when a score is NaN or outside `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("score {0} is outside 0.0..=1.0")]
pub struct InvalidScore(f32);

/// A value in `0.0..=1.0`: a PR's risk, or the similarity of a replayed diff to the original.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct Score(f32);

impl Score {
    /// The lowest score.
    pub const ZERO: Score = Score(0.0);
    /// The highest score.
    pub const ONE: Score = Score(1.0);

    /// Creates a score; rejects NaN and values outside `0.0..=1.0`.
    pub fn new(value: f32) -> Result<Self, InvalidScore> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidScore(value))
        }
    }

    /// Returns the score as a float.
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for Score {
    type Error = InvalidScore;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Score> for f32 {
    fn from(value: Score) -> Self {
        value.0
    }
}

impl fmt::Display for Score {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn rejects_out_of_range_and_nan() {
        for bad in [-0.01, 1.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(Score::new(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn accepts_bounds() {
        assert_eq!(Score::new(0.0), Ok(Score::ZERO));
        assert_eq!(Score::new(1.0), Ok(Score::ONE));
    }

    #[test]
    fn deserialization_validates() {
        assert_eq!(serde_json::from_str::<Score>("0.25").unwrap().get(), 0.25);
        assert!(serde_json::from_str::<Score>("1.5").is_err());
    }

    proptest! {
        #[test]
        fn new_accepts_exactly_the_unit_interval(v in any::<f32>()) {
            prop_assert_eq!(Score::new(v).is_ok(), (0.0..=1.0).contains(&v));
        }
    }
}
