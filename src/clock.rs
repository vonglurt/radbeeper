// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// Local time, which Rust's standard library does not have.
//
// std::time gives an instant and a duration and stops there: no calendar, no
// zone, no formatting. The Rust build has been printing `unix 1788600000`
// where the Python prints a timestamp, and that is fine for a line nobody
// parses -- but the log format's first column is a local ISO timestamp, and
// it is the column the whole format's sort order rests on.
//
// libc has all of it, and libc is the one dependency this crate already
// takes. strftime and strptime round-trip exactly, which is the property that
// matters: a row written here must read back here, and must read back in the
// Python, as the same second.
use std::ffi::CString;

/// Seconds since the epoch, now.
pub fn now() -> f64 {
    match std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
    {
        Ok(d) => d.as_secs_f64(),
        Err(_) => 0.0,
    }
}

// libc has deprecated `time_t` pending a width change on musl (libc#1848),
// but it is still the type localtime_r and mktime take, and there is no
// replacement to move to yet. Scoped to the two functions that cross into C
// rather than switched off for the file.
#[allow(deprecated)]
fn broken_down(t: i64) -> libc::tm {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let secs = t as libc::time_t;
        libc::localtime_r(&secs, &mut tm);
        tm
    }
}

/// strftime against local time. `t` is truncated to the second, as the log is.
pub fn format(t: f64, fmt: &str) -> String {
    let tm = broken_down(t.floor() as i64);
    let cf = match CString::new(fmt) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let mut buf = vec![0u8; 256];
    let n = unsafe {
        libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            cf.as_ptr(),
            &tm,
        )
    };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// The log's timestamp column, and the only shape written anywhere.
pub fn stamp(t: f64) -> String {
    format(t, "%Y-%m-%dT%H:%M:%S")
}

/// The other direction, for reading a log back.
///
/// tm_isdst is set to -1 so mktime works out for itself whether the local
/// clock was on summer time that day. Leaving it zero silently shifts every
/// row in half the year by an hour.
#[allow(deprecated)]
pub fn parse(text: &str, fmt: &str) -> Option<f64> {
    let cs = CString::new(text).ok()?;
    let cf = CString::new(fmt).ok()?;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_isdst = -1;
        let end = libc::strptime(cs.as_ptr(), cf.as_ptr(), &mut tm);
        if end.is_null() {
            return None;
        }
        let t = libc::mktime(&mut tm);
        if t == -1 {
            return None;
        }
        Some(t as f64)
    }
}

/// A local time built from its parts, as the counter's clock records them.
///
/// The fields are range-checked by the caller BEFORE this is reached, because
/// mktime normalises rather than refuses: month 99 day 99 is not an error to
/// it, it is 2034. Corrupt or half-erased flash throws up a timestamp marker
/// followed by rubbish, and a mark accepted from that would place every
/// sample after it in the wrong decade.
#[allow(deprecated)]
pub fn from_parts(year: i32, mon: i32, day: i32, hour: i32, min: i32, sec: i32)
    -> Option<f64>
{
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = year - 1900;
        tm.tm_mon = mon - 1;
        tm.tm_mday = day;
        tm.tm_hour = hour;
        tm.tm_min = min;
        tm.tm_sec = sec;
        tm.tm_isdst = -1;
        let t = libc::mktime(&mut tm);
        if t == -1 {
            None
        } else {
            Some(t as f64)
        }
    }
}

/// The timestamp a written row carries.
pub fn parse_stamp(text: &str) -> Option<f64> {
    parse(text, "%Y-%m-%dT%H:%M:%S")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_reads_back_as_the_second_it_was_written_for() {
        for t in [0.0, 1_000_000.0, 1_788_600_000.0, 2_000_000_000.0] {
            let s = stamp(t);
            assert_eq!(parse_stamp(&s), Some(t), "{} -> {}", t, s);
        }
    }

    #[test]
    fn a_stamp_is_iso_and_sorts_as_text() {
        let a = stamp(1_788_600_000.0);
        let b = stamp(1_788_600_001.0);
        assert_eq!(a.len(), 19);
        assert_eq!(&a[4..5], "-");
        assert_eq!(&a[10..11], "T");
        assert!(a < b, "{} should sort before {}", a, b);
        // A month later still sorts after, which is what the file names and
        // the row order both rely on.
        assert!(stamp(1_788_600_000.0) < stamp(1_791_600_000.0));
    }

    #[test]
    fn fractional_seconds_are_truncated_not_rounded() {
        // A row at 12:00:00.9 belongs to 12:00:00. Rounding would put it in
        // the next second and, at a month boundary, the next file.
        assert_eq!(stamp(1_788_600_000.9), stamp(1_788_600_000.0));
    }

    #[test]
    fn nonsense_is_none_rather_than_a_wrong_time() {
        assert_eq!(parse_stamp(""), None);
        assert_eq!(parse_stamp("not a time"), None);
        assert_eq!(parse_stamp("2026-13-45T99:99:99"), None);
        // An embedded NUL cannot become a C string, and must not panic.
        assert_eq!(parse_stamp("2026-09-04T\0"), None);
    }

    #[test]
    fn a_time_built_from_parts_is_the_time_it_names() {
        let t = from_parts(2026, 9, 4, 12, 0, 0).unwrap();
        assert_eq!(stamp(t), "2026-09-04T12:00:00");
        // mktime normalises, which is why the caller range-checks first.
        // This is here to record that it does, not to endorse it.
        assert!(from_parts(2026, 13, 45, 0, 0, 0).is_some());
    }

    #[test]
    fn the_month_a_row_belongs_to_is_its_own_local_month() {
        assert_eq!(format(1_788_600_000.0, "%Y-%m").len(), 7);
        assert_eq!(&format(1_788_600_000.0, "%Y-%m")[4..5], "-");
    }
}
