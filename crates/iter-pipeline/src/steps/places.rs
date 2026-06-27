//! PLACES — the addressed-POI index that backs place correlation
//! (`/places/related`, ADR 0012). No geocoder links a POI to its civico, so we
//! build the index: DuckDB reads Overture's `places` theme from public S3,
//! filtered to the discovery bbox and to places that carry an address, and
//! writes `output/places.jsonl` (id, name, category, freeform address, locality,
//! brand QID, lon/lat). The gateway loads it into an in-memory bucket index.
//!
//! Region-driven (`PLACES_BBOX` / the civici extent); no-ops without a bbox.
//! Skip-if-present; `FORCE_PLACES` re-extracts.

use std::path::Path;

use async_trait::async_trait;
use iter_contracts::BBox;
use iter_core::config;

use crate::context::Context;
use crate::step::Step;

const DEFAULT_OVERTURE_RELEASE: &str = "2026-06-17.0";

pub struct ExtractPlaces;

#[async_trait]
impl Step for ExtractPlaces {
    fn name(&self) -> &'static str {
        "PLACES"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        if ctx.discovery_bbox().is_none() {
            return true; // not applicable
        }
        ctx.output("output/places.jsonl").is_file()
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let Some(bbox_str) = ctx.discovery_bbox() else {
            tracing::info!("region declares no discovery bbox; skipping places");
            return Ok(());
        };
        let bbox = BBox::parse(&bbox_str)
            .map_err(|e| anyhow::anyhow!("invalid places bbox '{bbox_str}': {e}"))?;

        let out = ctx.output("output/places.jsonl");
        if let Some(parent) = out.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let release = config::or("CIVICI_OVERTURE_RELEASE", DEFAULT_OVERTURE_RELEASE);
        let ext_dir = config::or("DUCKDB_EXT_DIR", "/opt/duckdb/ext");
        let sql = places_sql(&release, &ext_dir, &bbox, &out);

        tracing::info!(release, bbox = %bbox_str, "extracting addressed POIs from Overture");
        let status = tokio::process::Command::new("duckdb")
            .arg("-no-stdin")
            .arg("-c")
            .arg(&sql)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "duckdb exited with {status}");
        anyhow::ensure!(
            out.is_file(),
            "duckdb reported success but places.jsonl is absent"
        );
        Ok(())
    }
}

/// The DuckDB script that reads Overture `places` by bbox and writes the
/// addressed-POI NDJSON the gateway loads. Pure, so the query shape is
/// unit-tested. The Overture `places.addresses[1].freeform` merges street +
/// number (the gateway splits it); `categories` is the pre-Sept-2026 field.
fn places_sql(release: &str, ext_dir: &str, bbox: &BBox, out: &Path) -> String {
    let src = format!("s3://overturemaps-us-west-2/release/{release}/theme=places/type=place/*");
    format!(
        "SET extension_directory='{ext_dir}';
LOAD spatial;
LOAD httpfs;
SET s3_region='us-west-2';
COPY (
  SELECT id,
         names.primary AS name,
         categories.primary AS category,
         addresses[1].freeform AS address,
         addresses[1].locality AS city,
         brand.wikidata AS brand_wikidata,
         ST_X(geometry) AS lon,
         ST_Y(geometry) AS lat
  FROM read_parquet('{src}', hive_partitioning=1)
  WHERE names.primary IS NOT NULL
    AND addresses[1].freeform IS NOT NULL
    AND bbox.xmin BETWEEN {min_lon} AND {max_lon}
    AND bbox.ymin BETWEEN {min_lat} AND {max_lat}
) TO '{out}' (FORMAT JSON, ARRAY false);",
        min_lon = bbox.min_lon,
        min_lat = bbox.min_lat,
        max_lon = bbox.max_lon,
        max_lat = bbox.max_lat,
        out = out.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_reads_places_and_emits_addressed_pois() {
        let bbox = BBox::parse("12.10,41.60,12.95,42.20").unwrap();
        let sql = places_sql(
            "2026-06-17.0",
            "/opt/duckdb/ext",
            &bbox,
            Path::new("/data/output/places.jsonl"),
        );
        assert!(sql.contains("theme=places/type=place"));
        assert!(sql.contains("release/2026-06-17.0/"));
        assert!(sql.contains("names.primary AS name"));
        assert!(sql.contains("categories.primary AS category"));
        assert!(sql.contains("addresses[1].freeform AS address"));
        assert!(sql.contains("brand.wikidata AS brand_wikidata"));
        assert!(sql.contains("ST_X(geometry) AS lon"));
        // only places carrying an address are indexable for correlation.
        assert!(sql.contains("addresses[1].freeform IS NOT NULL"));
        assert!(sql.contains("bbox.xmin BETWEEN 12.1 AND 12.95"));
        assert!(sql.contains("(FORMAT JSON, ARRAY false)"));
        assert!(sql.contains("/data/output/places.jsonl"));
    }
}
