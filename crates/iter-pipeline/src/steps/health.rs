//! HEALTH — write `output/health.json` from artifact mtimes. Not idempotent by
//! design: it always re-runs (unless `SKIP_HEALTH`) so the document reflects
//! the freshest build.

use std::path::Path;

use async_trait::async_trait;
use iter_contracts::health::StaticHealth;
use iter_core::Status;

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

pub struct WriteHealth;

#[async_trait]
impl Step for WriteHealth {
    fn name(&self) -> &'static str {
        "HEALTH"
    }

    async fn satisfied(&self, _ctx: &Context) -> bool {
        false
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let tiles_built_at =
            mtime_iso(&ctx.output(&format!("output/tiles/{}", ctx.tiles_filename()))).await;
        let gtfs_loaded = mtime_iso(&ctx.graph_dir().join("graph.obj"))
            .await
            .unwrap_or_else(|| "unknown".to_string());

        let status = if tiles_built_at.is_some() && gtfs_loaded != "unknown" {
            Status::Ok
        } else {
            Status::Degraded
        };

        let health = StaticHealth {
            status,
            version: ctx.version.clone(),
            gtfs_loaded,
            tiles_built_at,
            bootstrapped_at: Some(jiff::Timestamp::now().to_string()),
        };

        let bytes = serde_json::to_vec_pretty(&health)?;
        fsx::write_atomic(&ctx.output("output/health.json"), &bytes).await?;
        Ok(())
    }
}

async fn mtime_iso(path: &Path) -> Option<String> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    jiff::Timestamp::try_from(modified)
        .ok()
        .map(|t| t.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn ctx(data_dir: &std::path::Path, version: &str) -> Context {
        Context::for_test(data_dir.to_path_buf(), version)
    }

    #[tokio::test]
    async fn writes_degraded_when_artifacts_absent() {
        let dir = tempfile::tempdir().unwrap();
        WriteHealth.run(&ctx(dir.path(), "9.9.9")).await.unwrap();

        let bytes = std::fs::read(dir.path().join("output/health.json")).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "degraded");
        assert_eq!(v["version"], "9.9.9");
        assert_eq!(v["gtfsLoaded"], "unknown");
        assert_eq!(v["tilesBuiltAt"], Value::Null);
        assert!(v["bootstrappedAt"].is_string());
    }

    #[tokio::test]
    async fn writes_ok_when_artifacts_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("output/tiles")).unwrap();
        std::fs::create_dir_all(root.join("graph")).unwrap();
        std::fs::write(root.join("output/tiles/rome.pmtiles"), b"x").unwrap();
        std::fs::write(root.join("graph/graph.obj"), b"x").unwrap();

        WriteHealth.run(&ctx(root, "1.0.0")).await.unwrap();

        let bytes = std::fs::read(root.join("output/health.json")).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v["gtfsLoaded"].is_string());
        assert_ne!(v["gtfsLoaded"], "unknown");
        assert!(v["tilesBuiltAt"].is_string());
    }

    #[tokio::test]
    async fn never_satisfied_so_it_always_reruns() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!WriteHealth.satisfied(&ctx(dir.path(), "t")).await);
    }
}
