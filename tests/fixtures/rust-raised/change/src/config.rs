use std::collections::HashMap;
use std::time::Duration;

pub fn replica_count(settings: &HashMap<String, String>, key: &str) -> Option<usize> {
    /* OLD:collapsible_if:needs-newer */ if let Some(raw) = settings.get(key) {
        if let Ok(count) = raw.trim().parse::<usize>() {
            return Some(count);
        }
    }
    None
}

pub fn both(a: bool, b: bool) -> u8 {
    /* DECOY */ if a {
        if b {
            return 1;
        }
    }
    0
}

pub fn request_timeout(minutes: u64) -> Duration {
    Duration::from_secs(minutes * 60) // OLD:rust-duration-from-mins-hours:needs-newer
}

pub fn short_timeout() -> Duration {
    Duration::from_secs(5) // DECOY
}
