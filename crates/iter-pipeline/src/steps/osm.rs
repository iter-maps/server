//! OSM source fetch. One downloaded PBF feeds both the basemap render and the
//! transit clip. Skip-if-present; `FORCE_OSM` re-fetches.

use std::path::Path;

use async_trait::async_trait;
use iter_core::config;
use tokio::io::AsyncWriteExt;

use crate::context::Context;
use crate::step::Step;

const DEFAULT_OSM_URL: &str = "https://download.geofabrik.de/europe/italy/centro-latest.osm.pbf";

pub struct FetchOsm;

#[async_trait]
impl Step for FetchOsm {
    fn name(&self) -> &'static str {
        "OSM"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        ctx.output("sources/osm.pbf").is_file()
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let url = config::or("GEOFABRIK_OSM_URL", DEFAULT_OSM_URL);
        download(&ctx.http, &url, &ctx.output("sources/osm.pbf")).await
    }
}

/// Stream a URL to a file, written atomically (temp + rename).
async fn download(client: &reqwest::Client, url: &str, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut tmp = dest.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);

    tracing::info!(url, dest = %dest.display(), "downloading");
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
