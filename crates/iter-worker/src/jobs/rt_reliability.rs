//! GTFS-RT ingestion for historical reliability. Polls a `trip-updates` feed,
//! decodes it, and derives one delay event per stop, keyed on the stable tuple
//! (route, direction, stop, service-date) — never the raw `trip_id`, which Rome
//! renumbers near-daily. Garbage is dropped (|delay| over 2 h). Each derived
//! event is teed into the Tier-0 reliability store (ADR 0022); a store error is
//! logged and the poll continues — losing history is acceptable, crashing the
//! poll is not.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use iter_core::config;
use prost::Message;

use crate::gtfs_rt::FeedMessage;
use crate::job::Job;
use crate::reliability::store::Store;

/// Drop |delay| beyond this (seconds) — incoherent feed rows would poison a
/// percentile.
const MAX_ABS_DELAY_S: i32 = 2 * 60 * 60;

pub struct RtReliability {
    pub trip_updates_url: String,
    pub http: reqwest::Client,
    /// Root of the reliability rollup tree; events are teed into its Tier-0.
    pub reliability_dir: PathBuf,
}

impl RtReliability {
    /// Build from a feed's resolved trip-updates URL and the reliability dir;
    /// `RT_TRIP_UPDATES_URL` overrides the URL.
    pub fn new(trip_updates_url: String, http: reqwest::Client, reliability_dir: PathBuf) -> Self {
        Self {
            trip_updates_url: config::or("RT_TRIP_UPDATES_URL", &trip_updates_url),
            http,
            reliability_dir,
        }
    }
}

#[async_trait]
impl Job for RtReliability {
    fn name(&self) -> &'static str {
        "rt-reliability"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(config::parse("RT_POLL_SECS", 30))
    }

    async fn run(&self) -> anyhow::Result<()> {
        let bytes = self
            .http
            .get(&self.trip_updates_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let feed = FeedMessage::decode(bytes)?;
        let derived = derive_events(&feed);

        // Tee into the Tier-0 store. Fail-soft: a store error is logged and the
        // poll continues — losing a batch of history beats crashing the poll.
        let store = Store::new(&self.reliability_dir);
        tee_tier0(&store, &derived.events);

        tracing::info!(
            entities = feed.entity.len(),
            events = derived.events.len(),
            dropped = derived.dropped,
            feed_ts = feed.header.as_ref().and_then(|h| h.timestamp).unwrap_or(0),
            "rt: ingested trip-updates"
        );
        Ok(())
    }
}

/// Tee derived events into the Tier-0 store, swallowing and logging any store
/// error so the caller's poll always continues. A transient filesystem problem
/// must never crash the ingest loop — losing a batch of history is acceptable.
fn tee_tier0(store: &Store, events: &[StopEvent]) {
    if let Err(e) = store.append_tier0(events) {
        tracing::warn!(error = %e, "rt: tier-0 append failed (continuing)");
    }
}

/// The Europe/Rome wall-clock hour (0..=23) for a feed timestamp (epoch
/// seconds). Defaults to 12 (Midday) when the timestamp is absent or the zone
/// can't be loaded — a sensible neutral bucket rather than a panic.
fn rome_hour(feed_ts: Option<u64>) -> i32 {
    let Some(ts) = feed_ts else { return 12 };
    let Ok(secs) = i64::try_from(ts) else {
        return 12;
    };
    let Ok(tz) = jiff::tz::TimeZone::get("Europe/Rome") else {
        return 12;
    };
    match jiff::Timestamp::from_second(secs) {
        Ok(t) => t.to_zoned(tz).hour().into(),
        Err(_) => 12,
    }
}

/// One realized stop-delay observation, keyed on the stable tuple.
/// `feed_hour` is the Europe/Rome wall-clock hour of the observation, used to
/// derive the time-of-day rollup bucket — it is NOT part of the identity key.
#[derive(Debug, Clone, PartialEq)]
pub struct StopEvent {
    pub route_id: String,
    pub direction_id: i32,
    pub stop_id: String,
    pub service_date: String,
    pub delay_s: i32,
    pub feed_hour: i32,
}

pub struct Derived {
    pub events: Vec<StopEvent>,
    pub dropped: usize,
}

/// Flatten a feed into validated stop events. A stop's delay is its departure
/// delay (else arrival), falling back to the trip-level delay. Rows with no
/// route/stop, or an out-of-range delay, are dropped.
pub fn derive_events(feed: &FeedMessage) -> Derived {
    let mut events = Vec::new();
    let mut dropped = 0;
    let feed_hour = rome_hour(feed.header.as_ref().and_then(|h| h.timestamp));

    for entity in &feed.entity {
        let Some(tu) = &entity.trip_update else {
            continue;
        };
        let Some(trip) = &tu.trip else { continue };
        let (Some(route_id), Some(service_date)) = (&trip.route_id, &trip.start_date) else {
            dropped += tu.stop_time_update.len();
            continue;
        };
        let direction_id = trip.direction_id.unwrap_or(0);

        for stu in &tu.stop_time_update {
            let Some(stop_id) = &stu.stop_id else {
                dropped += 1;
                continue;
            };
            let delay = stu
                .departure
                .as_ref()
                .and_then(|e| e.delay)
                .or_else(|| stu.arrival.as_ref().and_then(|e| e.delay))
                .or(tu.delay);
            let Some(delay_s) = delay else {
                dropped += 1;
                continue;
            };
            if delay_s.abs() > MAX_ABS_DELAY_S {
                dropped += 1;
                continue;
            }
            events.push(StopEvent {
                route_id: route_id.clone(),
                direction_id,
                stop_id: stop_id.clone(),
                service_date: service_date.clone(),
                delay_s,
                feed_hour,
            });
        }
    }
    Derived { events, dropped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtfs_rt::{FeedEntity, StopTimeEvent, StopTimeUpdate, TripDescriptor, TripUpdate};

    fn feed(updates: Vec<TripUpdate>) -> FeedMessage {
        feed_at(None, updates)
    }

    fn feed_at(ts: Option<u64>, updates: Vec<TripUpdate>) -> FeedMessage {
        FeedMessage {
            header: ts.map(|t| crate::gtfs_rt::FeedHeader {
                gtfs_realtime_version: None,
                timestamp: Some(t),
            }),
            entity: updates
                .into_iter()
                .enumerate()
                .map(|(i, tu)| FeedEntity {
                    id: i.to_string(),
                    trip_update: Some(tu),
                })
                .collect(),
        }
    }

    fn stu(stop: &str, dep_delay: Option<i32>) -> StopTimeUpdate {
        StopTimeUpdate {
            stop_sequence: None,
            stop_id: Some(stop.to_string()),
            arrival: None,
            departure: dep_delay.map(|d| StopTimeEvent {
                delay: Some(d),
                time: None,
            }),
        }
    }

    fn trip(route: &str, dir: i32, date: &str, stus: Vec<StopTimeUpdate>) -> TripUpdate {
        TripUpdate {
            trip: Some(TripDescriptor {
                trip_id: Some("0#renumbered".into()),
                start_time: None,
                start_date: Some(date.into()),
                route_id: Some(route.into()),
                direction_id: Some(dir),
            }),
            stop_time_update: stus,
            timestamp: None,
            delay: None,
        }
    }

    #[test]
    fn derives_stable_keyed_events() {
        let f = feed(vec![trip(
            "MEA",
            0,
            "20260627",
            vec![stu("70001", Some(60)), stu("70002", Some(120))],
        )]);
        let d = derive_events(&f);
        assert_eq!(d.events.len(), 2);
        assert_eq!(d.dropped, 0);
        let e = &d.events[0];
        // keyed on route/direction/stop/date — NOT the renumbered trip_id.
        assert_eq!(e.route_id, "MEA");
        assert_eq!(e.stop_id, "70001");
        assert_eq!(e.service_date, "20260627");
        assert_eq!(e.delay_s, 60);
    }

    #[test]
    fn drops_garbage_delays_and_missing_fields() {
        let f = feed(vec![
            trip("MEB", 1, "20260627", vec![stu("X", Some(3 * 60 * 60))]), // > 2h → drop
            trip("MEB", 1, "20260627", vec![stu("Y", None)]),              // no delay → drop
        ]);
        let d = derive_events(&f);
        assert_eq!(d.events.len(), 0);
        assert_eq!(d.dropped, 2);
    }

    #[test]
    fn feed_hour_comes_from_the_header_timestamp_in_rome_wall_clock() {
        // 2026-06-21T06:30:00Z → 08:30 Europe/Rome (CEST, +2) → hour 8 (AM peak).
        let ts = 1_782_023_400u64;
        let f = feed_at(
            Some(ts),
            vec![trip("MEA", 0, "20260629", vec![stu("S", Some(0))])],
        );
        let d = derive_events(&f);
        assert_eq!(d.events[0].feed_hour, 8);
    }

    #[test]
    fn feed_hour_defaults_to_midday_without_a_timestamp() {
        let d = derive_events(&feed(vec![trip(
            "MEA",
            0,
            "20260629",
            vec![stu("S", Some(0))],
        )]));
        assert_eq!(d.events[0].feed_hour, 12);
    }

    #[test]
    fn append_tier0_errors_when_the_root_is_a_file() {
        // A store rooted under a regular file can't create its tier0 dir → Err.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let store = Store::new(blocker.join("reliability"));
        let events = vec![StopEvent {
            route_id: "MEA".into(),
            direction_id: 0,
            stop_id: "S".into(),
            service_date: "20260629".into(),
            delay_s: 30,
            feed_hour: 8,
        }];
        assert!(
            store.append_tier0(&events).is_err(),
            "append into an unwritable root must surface an error"
        );
    }

    #[test]
    fn tee_tier0_swallows_a_store_error_and_does_not_panic() {
        // The poll's fail-soft contract: a failing store is logged-and-continued,
        // never propagated or panicked.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let store = Store::new(blocker.join("reliability"));
        let events = vec![StopEvent {
            route_id: "MEA".into(),
            direction_id: 0,
            stop_id: "S".into(),
            service_date: "20260629".into(),
            delay_s: 30,
            feed_hour: 8,
        }];
        // Returns unit without panicking even though the underlying append fails.
        tee_tier0(&store, &events);
    }

    #[test]
    fn trip_level_delay_is_the_fallback() {
        let mut tu = trip("8", 0, "20260627", vec![stu("S1", None)]);
        tu.delay = Some(45);
        let d = derive_events(&feed(vec![tu]));
        assert_eq!(d.events.len(), 1);
        assert_eq!(d.events[0].delay_s, 45);
    }
}
