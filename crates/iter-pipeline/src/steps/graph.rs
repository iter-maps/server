//! GRAPH — build OTP's routing graph from the clipped OSM + GTFS feeds in the
//! base directory, saving `graph.obj` next to them. OTP serves that file with
//! `--load --serve`. The OTP jar lives in the data-prep image; we shell out to
//! it. No-ops for a basemap-only region. Skip-if-present; `FORCE_GRAPH`
//! rebuilds. Heap is sized by `OTP_BUILD_HEAP` (graph build is memory-hungry).

use std::path::Path;

use async_trait::async_trait;
use iter_core::config;

use crate::context::Context;
use crate::step::Step;

pub struct BuildGraph;

#[async_trait]
impl Step for BuildGraph {
    fn name(&self) -> &'static str {
        "GRAPH"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        let out = ctx.graph_dir().join("graph.obj");
        tokio::fs::metadata(&out)
            .await
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let dir = ctx.graph_dir();
        if !dir.join(ctx.clipped_osm_filename()).is_file() {
            tracing::info!("no clipped OSM; region is basemap-only, skipping graph build");
            return Ok(());
        }

        let jar = config::or("OTP_JAR", "/opt/otp.jar");
        let heap = config::or("OTP_BUILD_HEAP", "8g");
        let args = otp_build_args(&heap, &jar, &dir);

        tracing::info!(?args, "building OTP graph");
        let status = tokio::process::Command::new("java")
            .args(&args)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "OTP graph build exited with {status}");

        anyhow::ensure!(
            dir.join("graph.obj").is_file(),
            "OTP build reported success but graph.obj is absent"
        );
        Ok(())
    }
}

/// The OTP build argument vector: build the graph from the base directory and
/// save `graph.obj` into it. Pure, so the command shape is unit-tested even
/// though OTP runs in the build image.
fn otp_build_args(heap: &str, jar: &str, graph_dir: &Path) -> Vec<String> {
    vec![
        format!("-Xmx{heap}"),
        "-jar".to_string(),
        jar.to_string(),
        "--build".to_string(),
        "--save".to_string(),
        graph_dir.display().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_build_and_save_the_base_dir() {
        let args = otp_build_args("4g", "/opt/otp.jar", Path::new("/data/graph"));
        assert_eq!(args[0], "-Xmx4g");
        assert!(args.iter().any(|a| a == "--build"));
        assert!(args.iter().any(|a| a == "--save"));
        // the base dir is the final positional argument.
        assert_eq!(args.last().unwrap(), "/data/graph");
        // jar is invoked, not a class.
        let j = args.iter().position(|a| a == "-jar").unwrap();
        assert_eq!(args[j + 1], "/opt/otp.jar");
    }
}
