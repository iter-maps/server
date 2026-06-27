//! Minimal environment-variable helpers. Each service builds its own typed
//! config on top of these; the backend is configured entirely through env
//! (`.env` for "clone + up"), never through host-side state.

use std::str::FromStr;

pub fn opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

pub fn or(key: &str, default: &str) -> String {
    opt(key).unwrap_or_else(|| default.to_string())
}

/// Booleans accept `1/true/yes/on` (case-insensitive); anything else is false.
pub fn flag(key: &str, default: bool) -> bool {
    match opt(key) {
        None => default,
        Some(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
    }
}

pub fn parse<T: FromStr>(key: &str, default: T) -> T {
    opt(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}
