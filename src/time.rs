//! Wall-clock formatting for reports and job IDs (SPEC 13, 14).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch; times before it clamp to zero.
pub fn unix_seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// `YYYY-MM-DDTHH:MM:SSZ` in UTC.
pub fn rfc3339(t: SystemTime) -> String {
    let (date, (h, m, s)) = civil(unix_seconds(t));
    format!("{}T{h:02}:{m:02}:{s:02}Z", date)
}

/// A job ID sortable by start time: `YYYYMMDDTHHMMSSZ-<pid hex>`.
pub fn job_id(t: SystemTime, pid: u32) -> String {
    let (date, (h, m, s)) = civil(unix_seconds(t));
    format!("{}T{h:02}{m:02}{s:02}Z-{pid:x}", date.replace('-', ""))
}

/// A readable rootfs move folder name in UTC, to minute precision.
pub fn rootfs_move_dir_name(t: SystemTime) -> String {
    let (date, (h, m, _)) = civil(unix_seconds(t));
    format!("SynoReclaim-system-{}-{h:02}{m:02}", date.replace('-', ""))
}

fn civil(secs: u64) -> (String, (u64, u64, u64)) {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    (
        format!("{y:04}-{mo:02}-{d:02}"),
        (rem / 3600, rem % 3600 / 60, rem % 60),
    )
}

/// Howard Hinnant's days-to-civil algorithm (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// A short human age such as `42s`, `5m`, `3h`, or `2d`.
pub fn human_age(age: Duration) -> String {
    let s = age.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// An approximate duration for estimates: `<1 min`, `~15 min`,
/// `~2 h 5 min`.
pub fn human_duration(d: Duration) -> String {
    if d.as_secs() < 60 {
        return "<1 min".into();
    }
    let minutes = (d.as_secs() + 30) / 60;
    match minutes {
        0..=60 => format!("~{minutes} min"),
        _ if minutes % 60 == 0 => format!("~{} h", minutes / 60),
        _ => format!("~{} h {} min", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(rfc3339(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(at(1_790_000_000)), "2026-09-21T14:13:20Z");
        assert_eq!(rfc3339(at(951_782_400)), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn job_ids_sort_by_start_time() {
        assert_eq!(job_id(at(1_790_000_000), 0x1a2b), "20260921T141320Z-1a2b");
    }

    #[test]
    fn rootfs_move_folder_names_use_the_plan_time_in_utc() {
        assert_eq!(
            rootfs_move_dir_name(at(1_790_000_000)),
            "SynoReclaim-system-20260921-1413"
        );
    }

    #[test]
    fn ages_use_the_largest_whole_unit() {
        assert_eq!(human_age(Duration::from_secs(42)), "42s");
        assert_eq!(human_age(Duration::from_secs(300)), "5m");
        assert_eq!(human_age(Duration::from_secs(3 * 3600 + 5)), "3h");
        assert_eq!(human_age(Duration::from_secs(2 * 86_400)), "2d");
    }

    #[test]
    fn estimates_round_to_whole_minutes() {
        assert_eq!(human_duration(Duration::from_secs(59)), "<1 min");
        assert_eq!(human_duration(Duration::from_secs(911)), "~15 min");
        assert_eq!(human_duration(Duration::from_secs(3599)), "~60 min");
        assert_eq!(
            human_duration(Duration::from_secs(2 * 3600 + 300)),
            "~2 h 5 min"
        );
        assert_eq!(human_duration(Duration::from_secs(3 * 3600)), "~3 h");
    }
}
