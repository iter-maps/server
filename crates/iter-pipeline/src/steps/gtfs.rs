//! GTFS — download the region's enabled GTFS feeds into OTP's base directory,
//! one `<feedId>.gtfs.zip` per feed. `netex`-source feeds (FL) are converted by
//! the worker and don't belong here. Optional feeds warn on failure; required
//! feeds abort. `GTFS_FEEDS` narrows the set to a comma-separated allow-list
//! (handy to refresh one operator). Skip-if-present; `FORCE_GTFS` re-fetches.

use std::path::Path;

use async_trait::async_trait;
use iter_core::config;
use iter_region::Feed;
use tokio::io::AsyncWriteExt;

use crate::context::Context;
use crate::step::Step;

pub struct FetchGtfs;

#[async_trait]
impl Step for FetchGtfs {
    fn name(&self) -> &'static str {
        "GTFS"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        let feeds = targeted(ctx);
        if feeds.is_empty() {
            // Nothing to fetch; let run() log the no-op rather than reporting a
            // misleading "output present".
            return false;
        }
        for feed in feeds {
            let dest = ctx.graph_dir().join(gtfs_filename(&feed.id));
            if !dest.is_file() {
                return false;
            }
        }
        true
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let feeds = targeted(ctx);
        if feeds.is_empty() {
            tracing::info!("no GTFS feeds for this region; skipping");
            return Ok(());
        }

        let dir = ctx.graph_dir();
        tokio::fs::create_dir_all(&dir).await?;
        // Built only if a feed needs it — most don't, and skipping cert
        // verification is a documented, per-feed upstream gap (COTRAL).
        let mut insecure: Option<reqwest::Client> = None;

        for feed in feeds {
            let Some(url) = feed.url.as_deref() else {
                anyhow::bail!("feed {} has no url", feed.id);
            };
            let dest = dir.join(gtfs_filename(&feed.id));
            let client = if feed.insecure {
                insecure.get_or_insert_with(insecure_client)
            } else {
                &ctx.http
            };

            match download(client, url, &dest).await {
                Ok(()) => tracing::info!(feed = %feed.id, "fetched GTFS"),
                Err(e) if feed.optional => {
                    tracing::warn!(feed = %feed.id, error = %e, "optional GTFS feed failed; skipping")
                }
                Err(e) => {
                    return Err(e.context(format!("required GTFS feed {} failed", feed.id)));
                }
            }
        }
        Ok(())
    }
}

/// The enabled GTFS feeds this run targets: `netex` feeds are excluded (the
/// worker converts those), and `GTFS_FEEDS` — if set — narrows to a named set.
fn targeted(ctx: &Context) -> Vec<&Feed> {
    let allow = config::opt("GTFS_FEEDS");
    let allow: Option<Vec<&str>> = allow.as_deref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    });

    ctx.region
        .enabled_feeds()
        .filter(|f| is_gtfs(f))
        .filter(|f| {
            allow
                .as_ref()
                .is_none_or(|set| set.contains(&f.id.as_str()))
        })
        .collect()
}

/// A feed is GTFS unless it explicitly declares another source (`netex`).
fn is_gtfs(feed: &Feed) -> bool {
    matches!(feed.source.as_deref(), None | Some("gtfs"))
}

fn gtfs_filename(feed_id: &str) -> String {
    format!("{feed_id}.gtfs.zip")
}

fn insecure_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("iter-pipeline/", env!("CARGO_PKG_VERSION")))
        .danger_accept_invalid_certs(true)
        .build()
        .expect("insecure reqwest client")
}

/// Stream a URL to a file, written atomically (temp + rename).
async fn download(client: &reqwest::Client, url: &str, dest: &Path) -> anyhow::Result<()> {
    let mut tmp = dest.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);

    tracing::info!(url, dest = %dest.display(), "downloading GTFS");
    let mut resp = client.get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&tmp).await?;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtfs_feeds_are_recognized_netex_excluded() {
        let gtfs = Feed {
            id: "ATAC".into(),
            url: Some("https://x/atac.zip".into()),
            source: None,
            insecure: false,
            optional: false,
            enabled: None,
            license: None,
            realtime: vec![],
        };
        let netex = Feed {
            source: Some("netex".into()),
            ..gtfs.clone()
        };
        assert!(is_gtfs(&gtfs));
        assert!(!is_gtfs(&netex));
    }

    #[test]
    fn filename_carries_feed_id_and_gtfs_suffix() {
        assert_eq!(gtfs_filename("ATAC"), "ATAC.gtfs.zip");
    }

    #[test]
    fn committed_region_targets_gtfs_feeds_not_netex() {
        let ctx = Context::for_test(std::path::PathBuf::from("/tmp"), "test");
        let ids: Vec<&str> = targeted(&ctx).iter().map(|f| f.id.as_str()).collect();
        // ATAC + the two COTRAL feeds are GTFS; FL is netex (worker-built).
        assert!(ids.contains(&"ATAC"));
        assert!(ids.contains(&"COTRAL"));
        assert!(!ids.contains(&"TRENITALIA-FL"));
    }
}
