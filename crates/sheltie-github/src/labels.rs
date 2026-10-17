//! The labels Sheltie puts on pull requests and issues, with their colors and descriptions.

/// A label Sheltie manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label {
    /// Name, as shown on GitHub.
    pub name: &'static str,
    /// Color, six hex digits without `#`.
    pub color: &'static str,
    /// Hover text on GitHub.
    pub description: &'static str,
}

/// A pull request that landed through intent replay.
pub const REPLAYED: Label = Label {
    name: "sheltie: replayed",
    color: "8250df",
    description: "Sheltie re-derived this change from its intent and landed it",
};

/// A replayed change that differs too much from the reviewed one.
pub const NEEDS_REAPPROVAL: Label = Label {
    name: "sheltie: needs re-approval",
    color: "bf8700",
    description: "Sheltie re-derived this change; review the result before it can land",
};

/// An issue held until an overlapping issue is done.
pub const BLOCKED: Label = Label {
    name: "sheltie: blocked",
    color: "d4c5f9",
    description: "On hold: an overlapping issue is being worked on",
};

/// All of them.
pub const ALL: [Label; 3] = [REPLAYED, NEEDS_REAPPROVAL, BLOCKED];

/// The label with this name, if Sheltie manages it.
pub fn by_name(name: &str) -> Option<Label> {
    ALL.into_iter().find(|l| l.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_valid() {
        for l in ALL {
            assert!(l.name.len() <= 50 && l.description.len() <= 100, "{l:?}");
            assert!(l.color.len() == 6 && l.color.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(by_name(l.name), Some(l));
        }
    }
}
