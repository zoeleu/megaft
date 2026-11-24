//! Pure slot-selection logic: which bookable evaluation windows fit the user's constraints.

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

/// The intra hands out slots in 15-minute units; an evaluation takes `duration` of them.
pub const SLOT_MINUTES: i64 = 15;

/// A half-open time range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl Span {
    /// Whether the two ranges share any instant.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// The same range grown by `pad` on both sides.
    #[must_use]
    pub fn padded(&self, pad: SignedDuration) -> Self {
        Self {
            start: self.start - pad,
            end: self.end + pad,
        }
    }
}

/// One block of consecutive free 15-minute slots, as returned by `slots.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct SlotBlock {
    /// Comma-separated opaque slot ids, one per 15 minutes.
    pub ids: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// "Available" for bookable blocks.
    pub title: String,
}

impl SlotBlock {
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.title == "Available"
    }
}

/// A bookable evaluation: `duration` consecutive slot ids starting at `span.start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub span: Span,
    /// The ids to put in the booking URL, comma-separated.
    pub ids: String,
}

/// Length of an evaluation made of `duration` slots.
#[must_use]
pub fn evaluation_length(duration: u32) -> SignedDuration {
    SignedDuration::from_mins(SLOT_MINUTES.saturating_mul(i64::from(duration)))
}

/// Every bookable evaluation inside `[not_before, before)` that stays clear of `avoid`, earliest first.
///
/// A block of `n` slot ids offers `n - duration + 1` possible start times, 15 minutes apart.
#[must_use]
pub fn candidates(
    blocks: &[SlotBlock],
    duration: u32,
    not_before: Timestamp,
    before: Timestamp,
    avoid: &[Span],
) -> Vec<Candidate> {
    let length = evaluation_length(duration);
    let Ok(duration) = usize::try_from(duration) else {
        return Vec::new();
    };
    if duration == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for block in blocks.iter().filter(|b| b.is_available()) {
        let ids: Vec<&str> = block.ids.split(',').filter(|s| !s.is_empty()).collect();
        for (offset, window) in ids.windows(duration).enumerate() {
            let Ok(offset) = i64::try_from(offset) else { break };
            let start = block.start + SignedDuration::from_mins(SLOT_MINUTES.saturating_mul(offset));
            let span = Span {
                start,
                end: start + length,
            };
            if start < not_before || start >= before || avoid.iter().any(|a| a.overlaps(&span)) {
                continue;
            }
            out.push(Candidate {
                span,
                ids: window.join(","),
            });
        }
    }
    out.sort_by_key(|c| c.span.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn block(start: &str, n: usize) -> SlotBlock {
        let start = ts(start);
        let ids = (0..n).map(|i| format!("id{i}")).collect::<Vec<_>>().join(",");
        let end = start + SignedDuration::from_mins(15 * i64::try_from(n).unwrap());
        SlotBlock {
            ids,
            start,
            end,
            title: "Available".into(),
        }
    }

    #[test]
    fn expands_block_into_every_start_time() {
        let b = block("2026-11-24T12:00:00Z", 4); // 12:00-13:00 → starts 12:00, 12:15, 12:30
        let c = candidates(
            &[b],
            2,
            ts("2026-11-24T00:00:00Z"),
            ts("2026-11-25T00:00:00Z"),
            &[],
        );
        let starts: Vec<_> = c.iter().map(|c| c.span.start.to_string()).collect();
        assert_eq!(
            starts,
            [
                "2026-11-24T12:00:00Z",
                "2026-11-24T12:15:00Z",
                "2026-11-24T12:30:00Z"
            ]
        );
        assert_eq!(c[0].ids, "id0,id1");
        assert_eq!(c[2].ids, "id2,id3");
        assert_eq!(c[0].span.end, ts("2026-11-24T12:30:00Z"));
    }

    #[test]
    fn respects_window_and_avoid() {
        let b = block("2026-11-24T12:00:00Z", 8); // 12:00-14:00
        let avoid = [Span {
            start: ts("2026-11-24T12:15:00Z"),
            end: ts("2026-11-24T13:00:00Z"),
        }];
        let c = candidates(
            &[b],
            2,
            ts("2026-11-24T12:00:00Z"),
            ts("2026-11-24T13:30:00Z"),
            &avoid,
        );
        let starts: Vec<_> = c.iter().map(|c| c.span.start.to_string()).collect();
        // 12:00 overlaps 12:15; 13:00 and 13:15 are fine; 13:30 is not before the deadline.
        assert_eq!(starts, ["2026-11-24T13:00:00Z", "2026-11-24T13:15:00Z"]);
    }

    #[test]
    fn skips_unavailable_and_short_blocks() {
        let mut taken = block("2026-11-24T12:00:00Z", 4);
        taken.title = "Booked".into();
        let short = block("2026-11-24T15:00:00Z", 1);
        let c = candidates(
            &[taken, short],
            2,
            ts("2026-11-24T00:00:00Z"),
            ts("2026-11-25T00:00:00Z"),
            &[],
        );
        assert!(c.is_empty());
    }

    proptest! {
        #[test]
        fn every_candidate_is_inside_window_and_clear(
            offsets in proptest::collection::vec(0i64..96, 1..6),
            lens in proptest::collection::vec(1usize..10, 6),
            duration in 1u32..4,
            nb in 0i64..48, len in 1i64..48,
            avoid_at in 0i64..96, avoid_len in 0i64..8,
        ) {
            let day = ts("2026-11-24T00:00:00Z");
            let at = |q: i64| day + SignedDuration::from_mins(15 * q);
            let blocks: Vec<_> = offsets.iter().zip(&lens).map(|(o, n)| {
                let ids = (0..*n).map(|i| format!("b{o}s{i}")).collect::<Vec<_>>().join(",");
                SlotBlock { ids, start: at(*o), end: at(*o + i64::try_from(*n).unwrap()), title: "Available".into() }
            }).collect();
            let (not_before, before) = (at(nb), at(nb + len));
            let avoid = [Span { start: at(avoid_at), end: at(avoid_at + avoid_len) }];
            let out = candidates(&blocks, duration, not_before, before, &avoid);
            for c in &out {
                prop_assert!(c.span.start >= not_before && c.span.start < before);
                prop_assert!(!avoid.iter().any(|a| a.overlaps(&c.span)));
                prop_assert_eq!(c.ids.split(',').count(), duration as usize);
                prop_assert_eq!(c.span.end - c.span.start, evaluation_length(duration));
            }
            prop_assert!(out.windows(2).all(|w| w[0].span.start <= w[1].span.start));
        }
    }
}
