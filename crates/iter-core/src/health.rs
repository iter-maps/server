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
        Self {
            name: name.into(),
            status: Status::Ok,
            detail: None,
        }
    }

    pub fn down(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Down,
            detail: Some(detail.into()),
        }
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

    #[test]
    fn merge_truth_table() {
        use Status::{Degraded, Down, Ok};
        // Down dominates everything.
        assert_eq!(Down.merge(Down), Down);
        assert_eq!(Down.merge(Degraded), Down);
        assert_eq!(Down.merge(Ok), Down);
        assert_eq!(Degraded.merge(Down), Down);
        assert_eq!(Ok.merge(Down), Down);
        // Degraded dominates Ok but not Down.
        assert_eq!(Degraded.merge(Degraded), Degraded);
        assert_eq!(Degraded.merge(Ok), Degraded);
        assert_eq!(Ok.merge(Degraded), Degraded);
        // Ok only with Ok.
        assert_eq!(Ok.merge(Ok), Ok);
    }

    #[test]
    fn merge_is_commutative() {
        use Status::{Degraded, Down, Ok};
        for a in [Ok, Degraded, Down] {
            for b in [Ok, Degraded, Down] {
                assert_eq!(
                    a.merge(b),
                    b.merge(a),
                    "merge not commutative for {a:?},{b:?}"
                );
            }
        }
    }

    #[test]
    fn empty_checks_is_ok() {
        let r = Readiness::from_checks(vec![]);
        assert_eq!(r.status, Status::Ok);
        assert!(r.checks.is_empty());
    }

    #[test]
    fn any_degraded_but_no_down_is_degraded() {
        let mut degraded = Check::ok("cache");
        degraded.status = Status::Degraded;
        let r = Readiness::from_checks(vec![Check::ok("a"), degraded, Check::ok("b")]);
        assert_eq!(r.status, Status::Degraded);
    }

    #[test]
    fn down_wins_over_degraded() {
        let mut degraded = Check::ok("cache");
        degraded.status = Status::Degraded;
        let r = Readiness::from_checks(vec![degraded, Check::down("photon", "refused")]);
        assert_eq!(r.status, Status::Down);
    }

    #[test]
    fn from_checks_preserves_checks() {
        let r = Readiness::from_checks(vec![Check::ok("a"), Check::ok("b")]);
        assert_eq!(r.checks.len(), 2);
        assert_eq!(r.checks[0].name, "a");
        assert_eq!(r.checks[1].name, "b");
    }

    #[test]
    fn check_ok_constructor() {
        let c = Check::ok("otp");
        assert_eq!(c.name, "otp");
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.is_none());
    }

    #[test]
    fn check_down_constructor() {
        let c = Check::down("photon", "connection refused");
        assert_eq!(c.name, "photon");
        assert_eq!(c.status, Status::Down);
        assert_eq!(c.detail.as_deref(), Some("connection refused"));
    }

    #[test]
    fn status_serializes_lowercase() {
        assert_eq!(serde_json::to_value(Status::Ok).unwrap(), "ok");
        assert_eq!(serde_json::to_value(Status::Degraded).unwrap(), "degraded");
        assert_eq!(serde_json::to_value(Status::Down).unwrap(), "down");
    }

    #[test]
    fn check_serializes_with_lowercase_status() {
        let v = serde_json::to_value(Check::down("photon", "refused")).unwrap();
        assert_eq!(v["name"], "photon");
        assert_eq!(v["status"], "down");
        assert_eq!(v["detail"], "refused");
    }

    #[test]
    fn check_omits_detail_when_none() {
        let v = serde_json::to_value(Check::ok("otp")).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v.get("detail").is_none());
    }

    #[test]
    fn readiness_serializes_status_and_checks() {
        let r = Readiness::from_checks(vec![Check::ok("a")]);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["checks"][0]["name"], "a");
        assert_eq!(v["checks"][0]["status"], "ok");
    }
}
