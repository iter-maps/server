//! CLIP — carve the transit-routing street network out of the downloaded PBF
//! with osmium, over the region's `routing` extent. OTP builds its graph from
//! this clip, not the full-country PBF. `complete_ways` keeps ways that cross
//! the boundary whole, so the graph has no severed streets at the edge.
//! Skip-if-present; `FORCE_CLIP` re-clips. No-ops for a region without a transit
//! extent. osmium lives in the data-prep image; we shell out to it.

use std::path::Path;

use async_trait::async_trait;

use crate::context::Context;
use crate::step::Step;

pub struct ClipOsm;

#[async_trait]
impl Step for ClipOsm {
    fn name(&self) -> &'static str {
        "CLIP"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        let out = ctx.graph_dir().join(ctx.clipped_osm_filename());
        tokio::fs::metadata(&out)
            .await
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let Some(bounds) = ctx.routing_bounds() else {
            tracing::info!("no routing extent; region is basemap-only, skipping clip");
            return Ok(());
        };

        let src = ctx.output("sources/osm.pbf");
        anyhow::ensure!(
            src.is_file(),
            "OSM source missing; the OSM step must run first"
        );

        let dir = ctx.graph_dir();
        tokio::fs::create_dir_all(&dir).await?;
        // osmium infers the format from the output extension, so the temp file
        // keeps `.osm.pbf`; rename into place on success.
        let tmp = dir.join("clip-building.osm.pbf");
        let out = dir.join(ctx.clipped_osm_filename());

        let args = osmium_args(&bounds, &src, &tmp);
        tracing::info!(?args, "clipping street network with osmium");
        let status = tokio::process::Command::new("osmium")
            .args(&args)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "osmium exited with {status}");

        tokio::fs::rename(&tmp, &out).await?;
        Ok(())
    }
}

/// The osmium argument vector. Pure, so the command shape is unit-tested even
/// though osmium itself runs in the build image.
fn osmium_args(bbox: &str, src: &Path, out: &Path) -> Vec<String> {
    vec![
        "extract".to_string(),
        "-b".to_string(),
        bbox.to_string(),
        // keep boundary-crossing ways whole — severed streets break routing.
        "-s".to_string(),
        "complete_ways".to_string(),
        "--overwrite".to_string(),
        "-o".to_string(),
        out.display().to_string(),
        src.display().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_carry_bbox_strategy_and_paths() {
        let args = osmium_args(
            "11.3,41.1,14.05,43.35",
            Path::new("/d/sources/osm.pbf"),
            Path::new("/d/graph/rome.osm.pbf"),
        );
        assert_eq!(args[0], "extract");
        assert!(args.iter().any(|a| a == "11.3,41.1,14.05,43.35"));
        assert!(args.iter().any(|a| a == "complete_ways"));
        assert!(args.iter().any(|a| a == "--overwrite"));
        // input PBF is the last positional argument.
        assert_eq!(args.last().unwrap(), "/d/sources/osm.pbf");
        // output is passed via -o.
        let o = args.iter().position(|a| a == "-o").unwrap();
        assert_eq!(args[o + 1], "/d/graph/rome.osm.pbf");
    }
}
