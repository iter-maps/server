//! Generic liveness/readiness model. Concrete payloads (the static
//! `health.json` and the gateway health document) live in `iter-contracts`;
//! this is the shared status vocabulary and the readiness aggregation.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Degraded,
    Down,
}

impl Status {
    /// Worst-of: any `Down` makes the whole down; any `Degraded` degrades it.
    pub fn merge(self, other: Status) -> Status {
        match (self, other) {
            (Status::Down, _) | (_, Status::Down) => Status::Down,
            (Status::Degraded, _) | (_, Status::Degraded) => Status::Degraded,
            _ => Status::Ok,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Check {
    pub fn ok(name: impl Into<String>) -> Self {
        Self { name: name.into(), status: Status::Ok, detail: None }
    }

    pub fn down(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), status: Status::Down, detail: Some(detail.into()) }
    }
}

/// Aggregate readiness: the overall status is the worst of all checks.
#[derive(Debug, Clone, Serialize)]
pub struct Readiness {
    pub status: Status,
    pub checks: Vec<Check>,
}

impl Readiness {
    pub fn from_checks(checks: Vec<Check>) -> Self {
        let status = checks.iter().fold(Status::Ok, |acc, c| acc.merge(c.status));
        Self { status, checks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_is_worst_of_checks() {
        let r = Readiness::from_checks(vec![
            Check::ok("otp"),
            Check::down("photon", "connection refused"),
        ]);
        assert_eq!(r.status, Status::Down);
    }

    #[test]
    fn all_ok_is_ok() {
        let r = Readiness::from_checks(vec![Check::ok("a"), Check::ok("b")]);
        assert_eq!(r.status, Status::Ok);
    }
}
