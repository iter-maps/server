//! GTFS-RT ingestion for historical reliability. Polls a `trip-updates` feed,
//! decodes it, and derives one delay event per stop, keyed on the stable tuple
//! (route, direction, stop, service-date) — never the raw `trip_id`, which Rome
//! renumbers near-daily. Garbage is dropped (|delay| over 2 h). For now this
//! only ingests and summarizes; the persistent rollup tier lands next.

use std::time::Duration;

use async_trait::async_trait;
use iter_core::config;
use prost::Message;

use crate::gtfs_rt::FeedMessage;
use crate::job::Job;

/// Drop |delay| beyond this (seconds) — incoherent feed rows would poison a
/// percentile.
const MAX_ABS_DELAY_S: i32 = 2 * 60 * 60;

pub struct RtReliability {
    pub trip_updates_url: String,
    pub http: reqwest::Client,
}

impl RtReliability {
    /// Build from a feed's resolved trip-updates URL; `RT_TRIP_UPDATES_URL`
    /// overrides it.
    pub fn new(trip_updates_url: String, http: reqwest::Client) -> Self {
        Self {
            trip_updates_url: config::or("RT_TRIP_UPDATES_URL", &trip_updates_url),
            http,
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

        tracing::info!(
            entities = feed.entity.len(),
            events = derived.events.len(),
            dropped = derived.dropped,
            feed_ts = feed.header.as_ref().and_then(|h| h.timestamp).unwrap_or(0),
            "rt: ingested trip-updates"
        );
        // The Tier-0 append + rollup land with the reliability-archive ADR.
        Ok(())
    }
}

/// One realized stop-delay observation, keyed on the stable tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct StopEvent {
    pub route_id: String,
    pub direction_id: i32,
    pub stop_id: String,
    pub service_date: String,
    pub delay_s: i32,
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
        FeedMessage {
            header: None,
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
    fn trip_level_delay_is_the_fallback() {
        let mut tu = trip("8", 0, "20260627", vec![stu("S1", None)]);
        tu.delay = Some(45);
        let d = derive_events(&feed(vec![tu]));
        assert_eq!(d.events.len(), 1);
        assert_eq!(d.events[0].delay_s, 45);
    }
}
