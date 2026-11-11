//! Why a span was dropped from a batch.
//!
//! Spec 02 rule 21 (D111). The message is counts and codes only.

use std::collections::BTreeMap;
use std::fmt;

/// One closed reason a span was rejected.
///
/// A span records the first reason that applies, so [`Rejections::total`] is the
/// number of rejected spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RejectReason {
    /// Trace id missing, wrong length, or all zeros.
    InvalidTraceId,
    /// Span id missing, wrong length, or all zeros.
    InvalidSpanId,
    /// `start_time_unix_nano` or `end_time_unix_nano` above `i64::MAX`.
    TimestampOverflow,
    /// Start time outside the ingest window. Spec 02 rule 15, D85.
    StartTimeOutOfRange,
}

impl RejectReason {
    /// Snake_case code. Spec 02 rule 21.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTraceId => "invalid_trace_id",
            Self::InvalidSpanId => "invalid_span_id",
            Self::TimestampOverflow => "timestamp_overflow",
            Self::StartTimeOutOfRange => "start_time_out_of_range",
        }
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// How many spans were rejected, by reason.
///
/// The message lists at most [`MAX_REASONS`] codes. There are four reasons
/// today; the cap stays so a fifth does not make the text unbounded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Rejections {
    counts: BTreeMap<RejectReason, u64>,
}

/// Reasons named in [`Rejections::message`] before the remainder is collapsed.
const MAX_REASONS: usize = 4;

impl Rejections {
    pub(crate) fn record(&mut self, reason: RejectReason) {
        *self.counts.entry(reason).or_default() += 1;
    }

    /// Spans rejected. Each span contributes one.
    pub fn total(&self) -> u64 {
        self.counts.values().copied().sum()
    }

    /// Spans rejected for `reason`.
    pub fn count(&self, reason: RejectReason) -> u64 {
        self.counts.get(&reason).copied().unwrap_or(0)
    }

    /// `None` when nothing was rejected.
    ///
    /// Otherwise `rejected {total} span(s): {code} ×{n}, …`, reasons sorted by
    /// count descending then by code. At most four codes, then `, … ×{rest}`.
    pub fn message(&self) -> Option<String> {
        let rows = self
            .counts
            .iter()
            .map(|(reason, count)| (*reason, *count))
            .collect();
        render(self.total(), rows)
    }
}

fn render(total: u64, mut rows: Vec<(RejectReason, u64)>) -> Option<String> {
    if total == 0 {
        return None;
    }
    rows.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.as_str().cmp(right.0.as_str()))
    });
    let rest: u64 = rows.iter().skip(MAX_REASONS).map(|(_, count)| *count).sum();
    let word = if total == 1 { "span" } else { "spans" };
    let mut text = format!("rejected {total} {word}: ");
    for (index, (reason, count)) in rows.iter().take(MAX_REASONS).enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(reason.as_str());
        text.push_str(" ×");
        text.push_str(&count.to_string());
    }
    if rest > 0 {
        text.push_str(", … ×");
        text.push_str(&rest.to_string());
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(pairs: &[(RejectReason, u64)]) -> Rejections {
        let mut rejections = Rejections::default();
        for &(reason, count) in pairs {
            for _ in 0..count {
                rejections.record(reason);
            }
        }
        rejections
    }

    #[test]
    fn message_is_none_when_nothing_was_rejected() {
        assert_eq!(Rejections::default().total(), 0);
        assert_eq!(Rejections::default().message(), None);
        assert_eq!(Rejections::default().count(RejectReason::InvalidTraceId), 0);
    }

    #[test]
    fn message_uses_the_singular_and_no_request_data() {
        let rejections = recorded(&[(RejectReason::InvalidTraceId, 1)]);
        let message = rejections.message().expect("one rejection has a message");
        assert_eq!(message, "rejected 1 span: invalid_trace_id ×1");
        assert!(!message.contains("secret"));
        assert!(!message.contains("f00a"));
    }

    #[test]
    fn message_sorts_by_count_then_code() {
        let rejections = recorded(&[
            (RejectReason::StartTimeOutOfRange, 1),
            (RejectReason::InvalidSpanId, 2),
            (RejectReason::TimestampOverflow, 5),
        ]);
        assert_eq!(rejections.total(), 8);
        assert_eq!(rejections.count(RejectReason::TimestampOverflow), 5);
        assert_eq!(
            rejections.message().as_deref(),
            Some(
                "rejected 8 spans: timestamp_overflow ×5, invalid_span_id ×2, start_time_out_of_range ×1"
            )
        );

        let tied = recorded(&[
            (RejectReason::InvalidTraceId, 2),
            (RejectReason::InvalidSpanId, 2),
        ]);
        assert_eq!(
            tied.message().as_deref(),
            Some("rejected 4 spans: invalid_span_id ×2, invalid_trace_id ×2")
        );
    }

    #[test]
    fn message_collapses_reasons_past_the_cap() {
        let rows = vec![
            (RejectReason::TimestampOverflow, 5),
            (RejectReason::InvalidSpanId, 4),
            (RejectReason::InvalidTraceId, 3),
            (RejectReason::StartTimeOutOfRange, 2),
            (RejectReason::InvalidTraceId, 1),
        ];
        assert_eq!(
            render(15, rows).as_deref(),
            Some(
                "rejected 15 spans: timestamp_overflow ×5, invalid_span_id ×4, invalid_trace_id ×3, start_time_out_of_range ×2, … ×1"
            )
        );
    }
}
