//! BUILD_CONFIG — write OTP's `build-config.json` pinning the graph inputs
//! explicitly rather than relying on auto-scan. Each GTFS feed gets a stable
//! `feedId` (the client renders `FEED:LOCALID` ids by that prefix), and the
//! clipped OSM is named outright. Derived from what the CLIP and GTFS steps
//! actually left on disk, so the config never references a missing optional
//! feed. Always re-runs (cheap, and must track the current feed set).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

pub struct WriteBuildConfig;

#[async_trait]
impl Step for WriteBuildConfig {
    fn name(&self) -> &'static str {
        "BUILD_CONFIG"
    }

    async fn satisfied(&self, _ctx: &Context) -> bool {
        false
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let dir = ctx.graph_dir();
        let osm = ctx.clipped_osm_filename();
        if !dir.join(&osm).is_file() {
            tracing::info!("no clipped OSM; region is basemap-only, skipping build-config");
            return Ok(());
        }

        let mut feed_ids = gtfs_feed_ids(&dir).await?;
        feed_ids.sort();
        tracing::info!(osm = %osm, feeds = ?feed_ids, "writing OTP build-config");

        let cfg = build_config_json(&osm, &feed_ids);
        let bytes = serde_json::to_vec_pretty(&cfg)?;
        fsx::write_atomic(&dir.join("build-config.json"), &bytes).await?;
        Ok(())
    }
}

/// Scan the graph dir for `<feedId>.gtfs.zip` files and recover their feedIds.
async fn gtfs_feed_ids(dir: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if let Some(name) = entry.file_name().to_str()
            && let Some(id) = name.strip_suffix(".gtfs.zip")
        {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

/// The `build-config.json` body: pin the OSM input and each GTFS feed with an
/// explicit feedId. Pure, so the schema is unit-tested. Sources are filenames
/// resolved against OTP's base directory.
fn build_config_json(osm: &str, feed_ids: &[String]) -> Value {
    let transit: Vec<Value> = feed_ids
        .iter()
        .map(|id| json!({ "type": "gtfs", "feedId": id, "source": format!("{id}.gtfs.zip") }))
        .collect();

    json!({
        "osm": [ { "source": osm } ],
        "transitFeeds": transit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_osm_and_feeds_with_explicit_ids() {
        let cfg = build_config_json("rome.osm.pbf", &["ATAC".into(), "COTRAL".into()]);

        assert_eq!(cfg["osm"][0]["source"], "rome.osm.pbf");
        let feeds = cfg["transitFeeds"].as_array().unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0]["type"], "gtfs");
        assert_eq!(feeds[0]["feedId"], "ATAC");
        assert_eq!(feeds[0]["source"], "ATAC.gtfs.zip");
        assert_eq!(feeds[1]["feedId"], "COTRAL");
    }

    #[test]
    fn empty_feed_set_yields_no_transit() {
        let cfg = build_config_json("rome.osm.pbf", &[]);
        assert!(cfg["transitFeeds"].as_array().unwrap().is_empty());
        assert_eq!(cfg["osm"][0]["source"], "rome.osm.pbf");
    }
}
