//! Small numeric helpers shared by the catalog modules.

/// Turn a `u64` into the `i64` SQLite stores, saturating instead of wrapping.
pub(crate) fn to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

/// Turn a stored `i64` back into `u64`, clamping negatives to zero.
pub(crate) fn to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

/// Unix seconds, or `0` if the clock is before the epoch.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
