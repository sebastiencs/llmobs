//! Warning codes stored in `llmobs.normalize_warning`.
//!
//! Spec 02 rule 18. The normalizer writes the attribute from N4 on.
#![allow(dead_code)]

use std::collections::BTreeSet;

/// Recoverable oddity found while normalizing one span.
///
/// Variant order is alphabetical on [`Warning::code`], and `Ord` follows it.
/// [`Warnings::to_attribute`] relies on that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Warning {
    DuplicateKey,
    EndBeforeStart,
    InputExcludesCache,
    InvalidEventTime,
    InvalidNumber,
    MaxDepth,
}

impl Warning {
    /// Snake_case code written into the warning attribute.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::DuplicateKey => "duplicate_key",
            Self::EndBeforeStart => "end_before_start",
            Self::InputExcludesCache => "input_excludes_cache",
            Self::InvalidEventTime => "invalid_event_time",
            Self::InvalidNumber => "invalid_number",
            Self::MaxDepth => "max_depth",
        }
    }
}

/// Deduplicated warnings for one span, in [`Warning`] order.
#[derive(Debug, Default, Clone)]
pub(crate) struct Warnings(BTreeSet<Warning>);

impl Warnings {
    pub(crate) fn add(&mut self, warning: Warning) {
        self.0.insert(warning);
    }

    pub(crate) fn contains(&self, warning: Warning) -> bool {
        self.0.contains(&warning)
    }

    pub(crate) fn append(&mut self, other: Warnings) {
        self.0.extend(other.0);
    }

    /// `None` when empty. Otherwise a compact JSON array of codes, sorted.
    pub(crate) fn to_attribute(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        // Codes are snake_case identifiers, so they need no JSON escaping.
        let mut out = String::from("[");
        for (index, warning) in self.0.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(warning.code());
            out.push('"');
        }
        out.push(']');
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_order_are_alphabetical() {
        let ordered = [
            Warning::DuplicateKey,
            Warning::EndBeforeStart,
            Warning::InputExcludesCache,
            Warning::InvalidEventTime,
            Warning::InvalidNumber,
            Warning::MaxDepth,
        ];
        let mut by_code = ordered;
        by_code.sort_by_key(|warning| warning.code());
        assert_eq!(ordered, by_code);
        let mut by_ord = ordered;
        by_ord.sort();
        assert_eq!(ordered, by_ord);
        assert_eq!(
            ordered.map(Warning::code),
            [
                "duplicate_key",
                "end_before_start",
                "input_excludes_cache",
                "invalid_event_time",
                "invalid_number",
                "max_depth",
            ]
        );
    }

    #[test]
    fn to_attribute_is_a_sorted_json_array_without_duplicates() {
        let mut warnings = Warnings::default();
        assert_eq!(warnings.to_attribute(), None);

        warnings.add(Warning::MaxDepth);
        warnings.add(Warning::DuplicateKey);
        warnings.add(Warning::InvalidNumber);
        warnings.add(Warning::DuplicateKey);
        assert_eq!(
            warnings.to_attribute().as_deref(),
            Some("[\"duplicate_key\",\"invalid_number\",\"max_depth\"]")
        );
    }
}
