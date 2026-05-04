use crate::Result;
use chrono::offset::Utc;
use chrono::{DateTime, Timelike};

pub fn truncate_to_minute(dt: DateTime<Utc>) -> Result<DateTime<Utc>> {
    Ok(dt
        .with_nanosecond(0)
        .ok_or_else(|| format!("error setting nanoseconds to zero: {:?}", dt))?
        .with_second(0)
        .ok_or_else(|| format!("error setting seconds to zero: {:?}", dt))?)
}

pub fn now_seconds() -> Result<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("invalid duration {:?}", e))?
        .as_secs() as i64)
}

pub fn env_or(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    #[test]
    fn truncate_to_minute_clears_seconds_and_nanos() {
        let dt = Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 45).unwrap();
        let truncated = truncate_to_minute(dt).expect("truncate_to_minute should succeed");
        assert_eq!(truncated.second(), 0);
        assert_eq!(truncated.nanosecond(), 0);
        assert_eq!(truncated.minute(), 30);
        assert_eq!(truncated.hour(), 10);
    }

    #[test]
    fn truncate_to_minute_preserves_minute_and_above() {
        let dt = Utc.with_ymd_and_hms(2024, 3, 1, 23, 59, 59).unwrap();
        let truncated = truncate_to_minute(dt).unwrap();
        assert_eq!(truncated.year(), 2024);
        assert_eq!(truncated.month(), 3);
        assert_eq!(truncated.day(), 1);
        assert_eq!(truncated.hour(), 23);
        assert_eq!(truncated.minute(), 59);
        assert_eq!(truncated.second(), 0);
    }

    #[test]
    fn truncate_to_minute_already_at_minute_boundary() {
        let dt = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let truncated = truncate_to_minute(dt).unwrap();
        assert_eq!(truncated, dt);
    }

    #[test]
    fn now_seconds_returns_positive() {
        let secs = now_seconds().expect("now_seconds should not fail");
        // Unix epoch for 2020-01-01 is well above 0
        assert!(
            secs > 1_577_836_800,
            "now_seconds should be well past year 2020"
        );
    }

    #[test]
    fn now_seconds_is_monotonically_nondecreasing() {
        let t1 = now_seconds().unwrap();
        let t2 = now_seconds().unwrap();
        assert!(
            t2 >= t1,
            "second call to now_seconds should be >= first call"
        );
    }

    #[test]
    fn env_or_returns_default_for_missing_var() {
        // Use a key that is almost certainly not set
        let val = env_or("__COMMON_UTILS_TEST_MISSING_VAR__", "my-default");
        assert_eq!(val, "my-default");
    }

    #[test]
    fn env_or_returns_env_value_when_set() {
        let key = "__COMMON_UTILS_TEST_PRESENT_VAR__";
        std::env::set_var(key, "env-value");
        let val = env_or(key, "default-value");
        std::env::remove_var(key);
        assert_eq!(val, "env-value");
    }

    #[test]
    fn env_or_empty_default_returns_empty_string() {
        let val = env_or("__COMMON_UTILS_TEST_MISSING_VAR_2__", "");
        assert_eq!(val, "");
    }
}
