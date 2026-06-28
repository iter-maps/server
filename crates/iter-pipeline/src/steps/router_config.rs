//! ROUTER_CONFIG — write OTP's `router-config.json` next to the graph so the
//! served graph consumes the region's GTFS-RT feeds and journeys reflect live
//! delays. One updater per realtime channel that carries a url (ADR 0019): a
//! `stop-time-updater` for trip-updates (with fuzzy trip matching), a
//! `vehicle-positions`, a `real-time-alerts`. `feedId` is the feed id OTP keys
//! the graph by — the same id BUILD_CONFIG pins. A region with no realtime urls
//! gets an empty `updaters`, which OTP loads cleanly. Always re-runs (cheap, and
//! must track the current feed set).

use async_trait::async_trait;
use iter_region::Resolved;
use serde_json::{Value, json};

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

pub struct WriteRouterConfig;

#[async_trait]
impl Step for WriteRouterConfig {
    fn name(&self) -> &'static str {
        "ROUTER_CONFIG"
    }

    async fn satisfied(&self, _ctx: &Context) -> bool {
        false
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let dir = ctx.graph_dir();
        if !dir.join(ctx.clipped_osm_filename()).is_file() {
            tracing::info!("no clipped OSM; region is basemap-only, skipping router-config");
            return Ok(());
        }

        let cfg = router_config_json(&ctx.region);
        let n = cfg["updaters"].as_array().map(Vec::len).unwrap_or(0);
        tracing::info!(updaters = n, "writing OTP router-config");

        let bytes = serde_json::to_vec_pretty(&cfg)?;
        fsx::write_atomic(&dir.join("router-config.json"), &bytes).await?;
        Ok(())
    }
}

/// The `router-config.json` body: a GTFS-RT updater for every enabled feed
/// realtime channel that declares a url, keyed by the feed id. Pure, so the
/// schema is unit-tested even though OTP loads it in the serve image. Trip-update
/// and vehicle-position channels poll at 30 s, alerts at 60 s.
fn router_config_json(region: &Resolved) -> Value {
    let mut updaters = Vec::new();
    for feed in region.enabled_feeds() {
        if let Some(url) = feed.realtime_url("trip-updates") {
            updaters.push(json!({
                "type": "stop-time-updater",
                "feedId": feed.id,
                "url": url,
                "frequency": "30s",
                "fuzzyTripMatching": true,
            }));
        }
        if let Some(url) = feed.realtime_url("vehicle-positions") {
            updaters.push(json!({
                "type": "vehicle-positions",
                "feedId": feed.id,
                "url": url,
                "frequency": "30s",
            }));
        }
        if let Some(url) = feed.realtime_url("service-alerts") {
            updaters.push(json!({
                "type": "real-time-alerts",
                "feedId": feed.id,
                "url": url,
                "frequency": "60s",
            }));
        }
    }

    json!({ "updaters": updaters })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iter_region::{Civici, Extents, Feed, LiveTrains, RealtimeChannel};

    fn region(feeds: Vec<Feed>) -> Resolved {
        Resolved {
            target: "italy/lazio/rome".into(),
            id: "rome".into(),
            name: "Rome".into(),
            extents: Extents::default(),
            geocoding: None,
            live_trains: LiveTrains::default(),
            civici: Civici::default(),
            feeds,
            overlays: vec![],
        }
    }

    fn feed(id: &str, realtime: Vec<RealtimeChannel>) -> Feed {
        Feed {
            id: id.into(),
            url: None,
            source: None,
            insecure: false,
            optional: false,
            enabled: None,
            license: None,
            realtime,
        }
    }

    fn channel(channel: &str, url: Option<&str>) -> RealtimeChannel {
        RealtimeChannel {
            channel: channel.into(),
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn trip_updates_url_yields_one_stop_time_updater() {
        // The committed Rome ATAC shape: trip-updates has a url, the other two
        // channels are declared without one.
        let region = region(vec![feed(
            "ATAC",
            vec![
                channel("trip-updates", Some("https://example/atac-tu.pb")),
                channel("vehicle-positions", None),
                channel("service-alerts", None),
            ],
        )]);

        let cfg = router_config_json(&region);
        let updaters = cfg["updaters"].as_array().unwrap();
        assert_eq!(updaters.len(), 1);
        assert_eq!(updaters[0]["type"], "stop-time-updater");
        assert_eq!(updaters[0]["feedId"], "ATAC");
        assert_eq!(updaters[0]["url"], "https://example/atac-tu.pb");
        assert_eq!(updaters[0]["fuzzyTripMatching"], true);
        assert_eq!(updaters[0]["frequency"], "30s");
    }

    #[test]
    fn no_realtime_urls_yields_no_updaters() {
        // Channels declared without urls, plus a feed with no realtime at all.
        let region = region(vec![
            feed(
                "ATAC",
                vec![
                    channel("trip-updates", None),
                    channel("vehicle-positions", None),
                ],
            ),
            feed("COTRAL", vec![]),
        ]);

        let cfg = router_config_json(&region);
        assert!(cfg["updaters"].as_array().unwrap().is_empty());
    }

    #[test]
    fn all_three_channels_emit_their_typed_updaters() {
        let region = region(vec![feed(
            "ATAC",
            vec![
                channel("trip-updates", Some("https://example/tu.pb")),
                channel("vehicle-positions", Some("https://example/vp.pb")),
                channel("service-alerts", Some("https://example/al.pb")),
            ],
        )]);

        let cfg = router_config_json(&region);
        let updaters = cfg["updaters"].as_array().unwrap();
        assert_eq!(updaters.len(), 3);
        assert_eq!(updaters[0]["type"], "stop-time-updater");
        assert_eq!(updaters[1]["type"], "vehicle-positions");
        assert_eq!(updaters[1]["frequency"], "30s");
        assert_eq!(updaters[2]["type"], "real-time-alerts");
        assert_eq!(updaters[2]["frequency"], "60s");
    }

    #[test]
    fn disabled_feed_emits_no_updater() {
        let mut f = feed(
            "ATAC",
            vec![channel("trip-updates", Some("https://example/tu.pb"))],
        );
        f.enabled = Some(false);
        let cfg = router_config_json(&region(vec![f]));
        assert!(cfg["updaters"].as_array().unwrap().is_empty());
    }
}
