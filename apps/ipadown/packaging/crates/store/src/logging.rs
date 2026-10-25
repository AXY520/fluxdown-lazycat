//! 极简结构化日志（零第三方依赖）。
//!
//! 全部写 stdout（INFO）/ stderr（WARN、ERROR），Docker 与懒猫容器日志直接收集。
//! 行格式（单行、便于 grep）：
//!
//! ```text
//! 2026-09-27T08:30:12.345Z INFO  auth: login ok email=a@b.com
//! 2026-09-27T08:30:13.101Z ERROR ipatool: download failed bundle=com.x code=2fa
//! ```
//!
//! 使用 `log_info!` / `log_warn!` / `log_error!` 宏；target 用小写短标签
//! （http / auth / download / ipatool / config / ota / history）。

use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 同一时刻只输出一行，避免并发交错。
static LINE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO ",
            Self::Warn => "WARN ",
            Self::Error => "ERROR",
        }
    }
}

pub fn log(level: Level, target: &str, message: &str) {
    let line = format!(
        "{} {} {}: {}\n",
        iso8601_now(),
        level.as_str(),
        target,
        message
    );
    let _guard = LINE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    match level {
        Level::Info => {
            let _ = std::io::stdout().write_all(line.as_bytes());
        }
        Level::Warn | Level::Error => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }
}

/// Unix 时间戳 → `YYYY-MM-DDTHH:MM:SS.mmmZ`（UTC）。
fn iso8601_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = elapsed.as_secs() as i64;
    let millis = elapsed.subsec_millis();
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant 的民用日历换算（epoch 天数 → 年月日）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Unix 秒 → RFC 7231 IMF-fixdate（如 `Sun, 06 Nov 1994 08:49:37 GMT`），用于 Last-Modified。
pub fn httpdate_from_unix(secs: u64) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let weekday = WEEKDAYS[((days + 4).rem_euclid(7)) as usize]; // 1970-01-01 是周四（下标 4）
    let month = MONTHS[(month - 1) as usize];
    format!(
        "{weekday}, {day:02} {month} {year:04} {:02}:{:02}:{:02} GMT",
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60
    )
}

#[macro_export]
macro_rules! log_info {
    ($target:expr, $($arg:tt)+) => {
        $crate::logging::log($crate::logging::Level::Info, $target, &format!($($arg)+))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($target:expr, $($arg:tt)+) => {
        $crate::logging::log($crate::logging::Level::Warn, $target, &format!($($arg)+))
    };
}

#[macro_export]
macro_rules! log_error {
    ($target:expr, $($arg:tt)+) => {
        $crate::logging::log($crate::logging::Level::Error, $target, &format!($($arg)+))
    };
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, iso8601_now};

    #[test]
    fn formats_known_timestamps() {
        // 天数直接来自已验证的 epoch 换算
        let (year, month, day) = civil_from_days(20_723);
        assert_eq!((year, month, day), (2026, 9, 27));
        let (year, month, day) = civil_from_days(0);
        assert_eq!((year, month, day), (1970, 1, 1));
        // 闰日与次日
        let (year, month, day) = civil_from_days(19_782);
        assert_eq!((year, month, day), (2024, 2, 29));
        let (year, month, day) = civil_from_days(19_783);
        assert_eq!((year, month, day), (2024, 3, 1));
    }

    #[test]
    fn now_is_parseable_iso8601() {
        let now = iso8601_now();
        assert_eq!(now.len(), 24);
        assert!(now.ends_with('Z'));
        assert_eq!(&now[4..5], "-");
        assert_eq!(&now[10..11], "T");
    }

    #[test]
    fn formats_http_dates() {
        assert_eq!(
            super::httpdate_from_unix(0),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
        // 1994-11-06 08:49:37 GMT（RFC 7231 示例值）
        assert_eq!(
            super::httpdate_from_unix(784_111_777),
            "Sun, 06 Nov 1994 08:49:37 GMT"
        );
    }
}
