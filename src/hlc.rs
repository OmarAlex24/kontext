//! Hybrid logical clock (DESIGN §4.2).
//!
//! Text form: `YYYY-MM-DDTHH:MM:SS.mmmZ.CCCC` (UTC, 4-digit counter). It sorts
//! the same as text as it does as a value.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

const MAX_COUNTER: u32 = 9999;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    /// Milliseconds since 1970 (UTC).
    pub millis: i64,
    pub counter: u32,
}

impl Hlc {
    pub fn new(millis: i64, counter: u32) -> Hlc {
        Hlc { millis, counter }
    }

    /// The next value after this one. If the counter runs out, advance one
    /// millisecond.
    pub fn bump(self) -> Hlc {
        if self.counter >= MAX_COUNTER {
            Hlc::new(self.millis + 1, 0)
        } else {
            Hlc::new(self.millis, self.counter + 1)
        }
    }

    /// A new value, greater than `last` (the last one emitted or seen), using
    /// the physical time `now_millis` if it is ahead.
    pub fn next(now_millis: i64, last: Option<Hlc>) -> Hlc {
        match last {
            Some(last) if now_millis <= last.millis => last.bump(),
            _ => Hlc::new(now_millis, 0),
        }
    }

    /// Like [`Hlc::next`], with the system time (or `KONTEXT_NOW`, for tests).
    pub fn now(last: Option<Hlc>) -> Hlc {
        Hlc::next(crate::tasks::now_ms(), last)
    }

    /// The start of an ISO-8601 second or millisecond (`2026-11-04T01:11:54Z`).
    pub fn from_iso(s: &str) -> Result<Hlc> {
        let ts: Timestamp = s
            .trim()
            .parse()
            .with_context(|| format!("invalid date {s:?}"))?;
        Ok(Hlc::new(ts.as_millisecond(), 0))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ts = Timestamp::from_millisecond(self.millis).map_err(|_| fmt::Error)?;
        write!(
            f,
            "{}.{:03}Z.{:04}",
            ts.strftime("%Y-%m-%dT%H:%M:%S"),
            self.millis.rem_euclid(1000),
            self.counter
        )
    }
}

impl FromStr for Hlc {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Hlc> {
        let bad = || anyhow::anyhow!("invalid hlc {s:?}: expected YYYY-MM-DDTHH:MM:SS.mmmZ.CCCC");
        let (time, counter) = s.rsplit_once('.').ok_or_else(bad)?;
        if counter.len() != 4 || !counter.chars().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        let ts: Timestamp = time.parse().map_err(|_| bad())?;
        let hlc = Hlc::new(ts.as_millisecond(), counter.parse().map_err(|_| bad())?);
        // There is only one valid way to write each value.
        if hlc.to_string() != s {
            bail!(bad());
        }
        Ok(hlc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse() {
        let h: Hlc = "2026-11-03T18:22:10.123Z.0004".parse().unwrap();
        assert_eq!(h.counter, 4);
        assert_eq!(h.to_string(), "2026-11-03T18:22:10.123Z.0004");
        assert!("2026-11-03T18:22:10Z.0004".parse::<Hlc>().is_err());
        assert!("2026-11-03T18:22:10.123Z.4".parse::<Hlc>().is_err());
        assert!("2026-11-03T18:22:10.123+00:00.0004".parse::<Hlc>().is_err());
    }

    #[test]
    fn monotonic_when_clock_stalls_or_goes_back() {
        let mut last = None;
        let mut seen = Vec::new();
        // Physical time moves forward, stalls and goes back.
        for now in [1000, 1000, 1000, 999, 500, 1001, 1001, 2000] {
            let h = Hlc::next(now, last);
            if let Some(l) = last {
                assert!(h > l, "{h} must be greater than {l}");
            }
            last = Some(h);
            seen.push(h);
        }
        assert_eq!(seen[1], Hlc::new(1000, 1));
        assert_eq!(seen[4], Hlc::new(1000, 4));
        assert_eq!(seen[5], Hlc::new(1001, 0));
        assert_eq!(seen[7], Hlc::new(2000, 0));
    }

    #[test]
    fn text_order_matches_value_order() {
        let mut vals = [
            Hlc::new(1_700_000_000_000, 0),
            Hlc::new(1_700_000_000_000, 12),
            Hlc::new(1_700_000_000_001, 0),
            Hlc::new(1_800_000_000_000, 9999),
        ];
        let mut texts: Vec<String> = vals.iter().map(ToString::to_string).collect();
        vals.sort();
        texts.sort();
        assert_eq!(
            texts,
            vals.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
    }

    #[test]
    fn counter_overflow_moves_to_next_millisecond() {
        assert_eq!(Hlc::new(5, 9999).bump(), Hlc::new(6, 0));
    }

    #[test]
    fn from_v0_timestamp() {
        let h = Hlc::from_iso("2026-11-04T01:11:54Z").unwrap();
        assert_eq!(h.to_string(), "2026-11-04T01:11:54.000Z.0000");
    }
}
