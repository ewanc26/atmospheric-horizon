/// Current Unix time in seconds. A clock before the epoch is treated as zero;
/// the server does not need to fail over a broken clock.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
