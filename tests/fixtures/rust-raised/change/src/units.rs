use std::collections::HashMap;
use std::sync::OnceLock;

static FACTORS: OnceLock<HashMap<&'static str, u64>> = OnceLock::new(); // OLD:rust-oncelock-static-lazylock:needs-newer

fn factors() -> &'static HashMap<&'static str, u64> {
    FACTORS.get_or_init(|| HashMap::from([("kb", 1024), ("mb", 1024 * 1024)]))
}

pub static LAZY: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| 1); // OLD:incompatible_msrv:needs-newer

pub fn to_bytes(amount: u64, unit: &str) -> Option<u64> {
    factors().get(unit).map(|factor| factor * amount) // DECOY
}

static RUNTIME_UNIT: OnceLock<String> = OnceLock::new(); // DECOY

pub fn configure(unit: String) {
    let _ = RUNTIME_UNIT.set(unit);
}

pub fn unit() -> &'static str {
    RUNTIME_UNIT.get_or_init(|| "kb".to_string())
}
