//! Dates, as integers.
//!
//! The backtester buckets tens of thousands of transcript entries into weeks.
//! That needs three operations — parse an RFC 3339 date, find the Monday of its
//! week, print a date back — and nothing else. No timezone database, no
//! formatting language, no leap seconds.
//!
//! Howard Hinnant's `days_from_civil` is the whole calendar in a dozen lines of
//! integer arithmetic, exact for every proleptic Gregorian date. It is the same
//! judgement `bypass::age` already made when it refused a date crate for
//! "how long ago was this": a dependency here would be a tree of code to answer
//! a question that is genuinely arithmetic.
//!
//! ## Everything here is UTC
//!
//! Claude Code writes `"2026-08-19T14:02:11.123Z"` — fixed-width, zone-stamped,
//! and always Z. We bucket on that instant and never convert to local time. A
//! session that crosses local midnight lands in whichever UTC week it started;
//! at the resolution of a weekly bucket that is a rounding effect, and the
//! alternative is carrying a timezone database to move a handful of entries
//! across a boundary that is itself arbitrary. The backtest output says "UTC
//! weeks" out loud so nobody has to guess which it was.

/// Days since 1970-01-01. Signed because the arithmetic is, not because we
/// expect a transcript from 1969.
pub type Day = i64;

/// Days from the civil date, per Howard Hinnant's algorithm.
///
/// Shifts the year to start in March, which puts the leap day at the END of the
/// year and makes the month-length pattern regular enough to compute in one
/// expression. That is the trick; the rest is bookkeeping.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> Day {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The inverse, so a bucket can print its own label.
pub fn civil_from_days(z: Day) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The `YYYY-MM-DD` prefix of an RFC 3339 timestamp.
///
/// Deliberately positional rather than a parser: the field is machine-written
/// and fixed-width, so anything that is not exactly `dddd-dd-dd` is a line we
/// do not understand, and a line we do not understand is skipped rather than
/// guessed at.
pub fn day_of(ts: &str) -> Option<Day> {
    let b = ts.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let mut n: i64 = 0;
        for &c in &b[from..to] {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + i64::from(c - b'0');
        }
        Some(n)
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// The Monday on or before `day`.
///
/// 1970-01-01 was a Thursday, so `day + 3` puts Monday at a multiple of 7.
/// `rem_euclid` rather than `%` because the corpus can predate the epoch only
/// through a corrupt timestamp, and a negative remainder there would put the
/// bucket in the wrong week rather than failing loudly.
pub fn week_start(day: Day) -> Day {
    day - (day + 3).rem_euclid(7)
}

/// `YYYY-MM-DD`, for a bucket label.
pub fn iso(day: Day) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The algorithm is copied, so it is pinned against dates whose day number
    /// is known independently — the epoch itself, the Gregorian leap-century
    /// rule in both directions (2000 IS a leap year, 2100 is NOT), and a date
    /// far enough out that an off-by-one in the era arithmetic would show.
    #[test]
    fn the_calendar_agrees_with_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2000, 2, 29), 11016); // 2000 is a leap year
        assert_eq!(days_from_civil(2100, 3, 1), 47541);
        assert_eq!(days_from_civil(2100, 2, 28), 47540); // 2100 is NOT
        assert_eq!(days_from_civil(2026, 8, 20), 20685);
    }

    /// Round-tripping is what actually protects the bucket labels: an error in
    /// either direction alone would still print *a* date, just the wrong one.
    #[test]
    fn every_day_round_trips_through_the_calendar() {
        for day in -25_000..25_000 {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day, "{y:04}-{m:02}-{d:02}");
        }
    }

    #[test]
    fn a_timestamp_yields_the_day_its_prefix_names() {
        assert_eq!(day_of("2026-08-20T21:40:03.123Z"), Some(20685));
        assert_eq!(day_of("2026-08-20"), Some(20685));
    }

    /// A truncated or reordered timestamp is a line we cannot place in time.
    /// Placing it anyway — at the epoch, or at today — would silently move
    /// entries between buckets, which is the one thing the backtest must not do.
    #[test]
    fn an_unparseable_timestamp_is_not_a_guess() {
        assert_eq!(day_of(""), None);
        assert_eq!(day_of("2026-08"), None);
        assert_eq!(day_of("20260820T00:00:00Z"), None);
        assert_eq!(day_of("not-a-date"), None);
        assert_eq!(day_of("2026-13-01"), None);
        assert_eq!(day_of("2026-00-01"), None);
        assert_eq!(day_of("2026-08-32"), None);
    }

    /// The table in the plan is keyed on "week starting <Monday>". A bucket
    /// boundary that drifted by a day would silently reshape every rate.
    #[test]
    fn a_week_starts_on_monday() {
        // 2026-08-20 is a Thursday; its week starts Monday 2026-08-17.
        assert_eq!(iso(week_start(day_of("2026-08-20").unwrap())), "2026-08-17");
        // A Monday is its own week start.
        assert_eq!(iso(week_start(day_of("2026-08-17").unwrap())), "2026-08-17");
        // A Sunday belongs to the week that began six days earlier.
        assert_eq!(iso(week_start(day_of("2026-08-23").unwrap())), "2026-08-17");
        // The day after that Sunday opens a new bucket.
        assert_eq!(iso(week_start(day_of("2026-08-24").unwrap())), "2026-08-24");
    }

    /// `%` would return a negative remainder for a pre-epoch day and push the
    /// bucket a week forward. Only a corrupt timestamp can get here, but a
    /// corrupt timestamp should not produce a plausible-looking wrong bucket.
    #[test]
    fn a_pre_epoch_day_still_lands_on_a_monday() {
        for day in -400..400 {
            let start = week_start(day);
            assert!(start <= day && day - start < 7, "day {day} → {start}");
            assert_eq!(week_start(start), start, "week_start is idempotent");
        }
    }
}
