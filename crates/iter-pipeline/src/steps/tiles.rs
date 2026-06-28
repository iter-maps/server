//! Basemap tiles via planetiler — renders the OSM PBF into a clustered PMTiles
//! v3 archive (OMT schema, z0-14) over the region's basemap extent. planetiler
//! lives in the data-prep image; we shell out to it. Skip-if-present;
//! `FORCE_TILES` rebuilds.

use std::path::Path;

use async_trait::async_trait;
use iter_core::config;

use crate::context::Context;
use crate::step::Step;

pub struct BuildTiles;

#[async_trait]
impl Step for BuildTiles {
    fn name(&self) -> &'static str {
        "TILES"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        let out = ctx.output(&format!("output/tiles/{}", ctx.tiles_filename()));
        tokio::fs::metadata(&out)
            .await
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let src = ctx.output("sources/osm.pbf");
        anyhow::ensure!(
            src.is_file(),
            "OSM source missing; the OSM step must run first"
        );

        let out_dir = ctx.output("output/tiles");
        tokio::fs::create_dir_all(&out_dir).await?;
        // planetiler infers the format from the `.pmtiles` extension, so the
        // temp file must keep it; rename into place on success.
        let tmp = out_dir.join("tiles-building.pmtiles");
        let out = out_dir.join(ctx.tiles_filename());

        let jar = config::or("PLANETILER_JAR", "/opt/planetiler.jar");
        let heap = config::or("PLANETILER_HEAP", "3g");
        let args = planetiler_args(&heap, &jar, &src, &tmp, ctx.basemap_bounds().as_deref());

        tracing::info!(?args, "running planetiler");
        let status = tokio::process::Command::new("java")
            .args(&args)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "planetiler exited with {status}");

        tokio::fs::rename(&tmp, &out).await?;
        Ok(())
    }
}

/// The planetiler argument vector. Pure, so the command shape is unit-tested
/// even though planetiler itself runs in the build image.
fn planetiler_args(
    heap: &str,
    jar: &str,
    src: &Path,
    out: &Path,
    bounds: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        format!("-Xmx{heap}"),
        "-jar".to_string(),
        jar.to_string(),
        format!("--osm-path={}", src.display()),
        // fetch any missing ancillaries (water polygons, natural earth)
        "--download".to_string(),
        "--force".to_string(),
        format!("--output={}", out.display()),
        "--minzoom=0".to_string(),
        "--maxzoom=14".to_string(),
    ];
    if let Some(b) = bounds {
        args.push(format!("--bounds={b}"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_carry_osm_output_zoom_and_bounds() {
        let args = planetiler_args(
            "4g",
            "/opt/planetiler.jar",
            Path::new("/d/sources/osm.pbf"),
            Path::new("/d/out.pmtiles"),
            Some("11.3,41.1,14.05,43.35"),
        );
        assert!(args.contains(&"-Xmx4g".to_string()));
        assert!(args.iter().any(|a| a == "--osm-path=/d/sources/osm.pbf"));
        assert!(args.iter().any(|a| a == "--output=/d/out.pmtiles"));
        assert!(args.iter().any(|a| a == "--maxzoom=14"));
        assert!(args.iter().any(|a| a == "--bounds=11.3,41.1,14.05,43.35"));
    }

    #[test]
    fn bounds_omitted_when_none() {
        let args = planetiler_args("2g", "/j", Path::new("/s"), Path::new("/o.pmtiles"), None);
        assert!(!args.iter().any(|a| a.starts_with("--bounds")));
    }
}
