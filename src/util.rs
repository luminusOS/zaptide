//! Small formatting helpers shared by the views.

use jiff::civil::{Date, DateTime};
use jiff::{Timestamp, Zoned};

fn zoned(unix_seconds: i64) -> Option<Zoned> {
    let timestamp = Timestamp::from_second(unix_seconds).ok()?;
    Some(timestamp.to_zoned(jiff::tz::TimeZone::system()))
}

fn today() -> Date {
    Zoned::now().date()
}

/// Local message time such as "14:05".
pub fn clock(unix_seconds: i64) -> String {
    zoned(unix_seconds)
        .map(|when| clock_at(when.datetime()))
        .unwrap_or_default()
}

fn clock_at(when: DateTime) -> String {
    when.strftime("%H:%M").to_string()
}

/// WhatsApp transcript timestamp such as `22:41, 8/18/2026`.
pub fn copy_stamp(unix_seconds: i64) -> String {
    zoned(unix_seconds)
        .map(|when| copy_stamp_at(when.datetime()))
        .unwrap_or_default()
}

fn copy_stamp_at(when: DateTime) -> String {
    when.strftime("%-H:%M, %-m/%-d/%-Y").to_string()
}

/// Message-info timestamp with date and minute.
pub fn moment_stamp(unix_seconds: i64) -> String {
    let Some(when) = zoned(unix_seconds) else {
        return String::new();
    };
    let time = clock_at(when.datetime());
    let days = today()
        .since(when.date())
        .map(|span| span.get_days())
        .unwrap_or(i32::MAX);
    match days {
        0 => time,
        1 => format!("Yesterday at {time}"),
        2..=6 => format!("{} at {time}", weekday_name(when.date())),
        _ => format!("{} at {time}", short_date(when.date())),
    }
}

/// Conversation day-separator label.
pub fn day_label(unix_seconds: i64) -> String {
    let Some(when) = zoned(unix_seconds) else {
        return String::new();
    };
    let date = when.date();
    let today = today();
    let days = today
        .since(date)
        .map(|span| span.get_days())
        .unwrap_or(i32::MAX);
    match days {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => weekday_name(date),
        _ => long_date(date),
    }
}

/// Whether two Unix times fall on the same local calendar day.
pub fn same_day(first: i64, second: i64) -> bool {
    match (zoned(first), zoned(second)) {
        (Some(first), Some(second)) => first.date() == second.date(),
        _ => first == second,
    }
}

#[cfg(test)]
pub fn noon_today() -> i64 {
    today()
        .at(12, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::system())
        .map(|noon| noon.timestamp().as_second())
        .unwrap_or_else(|_| now())
}

fn weekday_name(date: Date) -> String {
    date.strftime("%A").to_string()
}

fn short_date(date: Date) -> String {
    date.strftime("%-d %b %-Y").to_string()
}

fn long_date(date: Date) -> String {
    date.strftime("%A, %-d %B %-Y").to_string()
}

/// The current time as a Unix timestamp.
pub fn now() -> i64 {
    Timestamp::now().as_second()
}

/// Duration such as "0:12".
pub fn duration(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// File size in SI units, localized by GLib, such as "1.2 MB".
pub fn bytes(size: u64) -> String {
    gtk4::glib::format_size(size).into()
}

/// Formats a phone number with a plus sign and grouped digits.
pub fn phone(digits: &str) -> String {
    let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return String::new();
    }
    let mut out = String::from("+");
    for (index, character) in digits.chars().enumerate() {
        // Approximate a country code followed by groups of three digits.
        if index == 2 || (index > 2 && (index - 2) % 3 == 0) {
            out.push(' ');
        }
        out.push(character);
    }
    out
}

/// Stable id-derived avatar hue.
pub fn hue(seed: &str) -> f32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in seed.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash % 360) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact output for each weekday and month, a leap day, single-digit days
    /// and months, midnight, noon, 23:59, year boundaries and short years.
    #[test]
    fn dates_and_times_keep_their_exact_wording() {
        type Golden = (
            (i16, i8, i8, i8, i8),
            &'static str,
            &'static str,
            &'static str,
            &'static str,
        );
        let goldens: &[Golden] = &[
            (
                (2024, 2, 29, 0, 0),
                "00:00",
                "0:00, 2/29/2024",
                "29 Feb 2024",
                "Thursday, 29 February 2024",
            ),
            (
                (2024, 1, 1, 0, 0),
                "00:00",
                "0:00, 1/1/2024",
                "1 Jan 2024",
                "Monday, 1 January 2024",
            ),
            (
                (2023, 12, 31, 23, 59),
                "23:59",
                "23:59, 12/31/2023",
                "31 Dec 2023",
                "Sunday, 31 December 2023",
            ),
            (
                (2026, 1, 5, 12, 0),
                "12:00",
                "12:00, 1/5/2026",
                "5 Jan 2026",
                "Monday, 5 January 2026",
            ),
            (
                (2026, 2, 3, 9, 7),
                "09:07",
                "9:07, 2/3/2026",
                "3 Feb 2026",
                "Tuesday, 3 February 2026",
            ),
            (
                (2026, 3, 4, 13, 30),
                "13:30",
                "13:30, 3/4/2026",
                "4 Mar 2026",
                "Wednesday, 4 March 2026",
            ),
            (
                (2026, 4, 9, 1, 1),
                "01:01",
                "1:01, 4/9/2026",
                "9 Apr 2026",
                "Thursday, 9 April 2026",
            ),
            (
                (2026, 5, 15, 11, 59),
                "11:59",
                "11:59, 5/15/2026",
                "15 May 2026",
                "Friday, 15 May 2026",
            ),
            (
                (2026, 6, 20, 12, 1),
                "12:01",
                "12:01, 6/20/2026",
                "20 Jun 2026",
                "Saturday, 20 June 2026",
            ),
            (
                (2026, 7, 26, 18, 45),
                "18:45",
                "18:45, 7/26/2026",
                "26 Jul 2026",
                "Sunday, 26 July 2026",
            ),
            (
                (2026, 8, 18, 22, 41),
                "22:41",
                "22:41, 8/18/2026",
                "18 Aug 2026",
                "Tuesday, 18 August 2026",
            ),
            (
                (2026, 9, 30, 6, 5),
                "06:05",
                "6:05, 9/30/2026",
                "30 Sep 2026",
                "Wednesday, 30 September 2026",
            ),
            (
                (2026, 10, 10, 10, 10),
                "10:10",
                "10:10, 10/10/2026",
                "10 Oct 2026",
                "Saturday, 10 October 2026",
            ),
            (
                (2026, 11, 1, 0, 59),
                "00:59",
                "0:59, 11/1/2026",
                "1 Nov 2026",
                "Sunday, 1 November 2026",
            ),
            (
                (2026, 12, 25, 23, 0),
                "23:00",
                "23:00, 12/25/2026",
                "25 Dec 2026",
                "Friday, 25 December 2026",
            ),
            (
                (2000, 2, 29, 12, 0),
                "12:00",
                "12:00, 2/29/2000",
                "29 Feb 2000",
                "Tuesday, 29 February 2000",
            ),
            (
                (1970, 1, 1, 0, 0),
                "00:00",
                "0:00, 1/1/1970",
                "1 Jan 1970",
                "Thursday, 1 January 1970",
            ),
            (
                (999, 7, 4, 5, 6),
                "05:06",
                "5:06, 7/4/999",
                "4 Jul 999",
                "Thursday, 4 July 999",
            ),
            (
                (9999, 12, 31, 23, 59),
                "23:59",
                "23:59, 12/31/9999",
                "31 Dec 9999",
                "Friday, 31 December 9999",
            ),
        ];
        for &((year, month, day, hour, minute), clock, copy, short, long) in goldens {
            let when = jiff::civil::date(year, month, day).at(hour, minute, 0, 0);
            assert_eq!(clock_at(when), clock, "{when}");
            assert_eq!(copy_stamp_at(when), copy, "{when}");
            assert_eq!(short_date(when.date()), short, "{when}");
            assert_eq!(long_date(when.date()), long, "{when}");
            assert_eq!(
                weekday_name(when.date()),
                long.split(',').next().unwrap(),
                "{when}"
            );
        }
    }

    #[test]
    fn phone_numbers_are_grouped() {
        assert_eq!(phone("393331234567"), "+39 333 123 456 7");
        assert_eq!(phone("15551234567"), "+15 551 234 567");
        assert_eq!(phone(""), "");
    }

    #[test]
    fn sizes_and_durations_read_naturally() {
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(512), "512 bytes");
        assert_eq!(bytes(2_048), "2.0\u{a0}kB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.2\u{a0}MB");
        assert_eq!(duration(75), "1:15");
    }
}
