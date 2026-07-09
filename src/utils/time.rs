//! 时间工具：统一时间获取与格式化

use chrono::{DateTime, Utc};

/// 当前 UTC 时间
pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

/// 当前 UTC 时间 RFC3339 字符串
pub fn now_utc_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// 当前毫秒时间戳
pub fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// 当前秒时间戳
pub fn now_secs() -> i64 {
    Utc::now().timestamp()
}
