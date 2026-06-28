//! CIVICI — Italian house numbers, the geocoding enrichment that fixes
//! "Via Tripoli 20" dropping Rome (OSM has almost no civici; the index can't
//! bias to a number it never held). We read Overture's `addresses` theme
//! straight from public S3 with DuckDB, filtered to the region's `civici` bbox,
//! and emit header-less Photon "house" docs (`object_type: itermaps:civico`,
//! importance 0.00005 so location bias — not a fake-high score — picks the right
//! #20). The PHOTON step appends these to the import stream.
//!
//! Region-driven (`region.civici`); a region without a civici bbox is a no-op,
//! and any stale `civici.jsonl` is removed so the next import omits it.
//! Skip-if-present; `FORCE_CIVICI` re-extracts (pair with `FORCE_PHOTON` to
//! reach the served index — the import is all-or-nothing).

use std::path::Path;

use async_trait::async_trait;
use iter_contracts::BBox;
use iter_core::config;

use crate::context::Context;
use crate::step::Step;

/// Overture release to read. Files are retained ~60 days, so this is bumped as
/// releases roll; `CIVICI_OVERTURE_RELEASE` overrides.
const DEFAULT_OVERTURE_RELEASE: &str = "2026-06-17.0";

pub struct ExtractCivici;

#[async_trait]
impl Step for ExtractCivici {
    fn name(&self) -> &'static str {
        "CIVICI"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        // Not applicable → "satisfied" so the runner moves on quietly; the
        // not-applicable cleanup happens in run() when forced/first-run.
        if !ctx.civici_enabled() || ctx.civici_bbox().is_none() {
            return true;
        }
        ctx.photon_dir().join("civici.jsonl").is_file()
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let out = ctx.photon_dir().join("civici.jsonl");

        if !ctx.civici_enabled() {
            tracing::info!("civici disabled; removing any stale civici.jsonl");
            let _ = tokio::fs::remove_file(&out).await;
            return Ok(());
        }
        let Some(bbox_str) = ctx.civici_bbox() else {
            tracing::info!("region declares no civici bbox; skipping");
            let _ = tokio::fs::remove_file(&out).await;
            return Ok(());
        };
        let bbox = BBox::parse(&bbox_str)
            .map_err(|e| anyhow::anyhow!("invalid civici bbox '{bbox_str}': {e}"))?;

        tokio::fs::create_dir_all(ctx.photon_dir()).await?;
        let release = config::or("CIVICI_OVERTURE_RELEASE", DEFAULT_OVERTURE_RELEASE);
        let ext_dir = config::or("DUCKDB_EXT_DIR", "/opt/duckdb/ext");
        let country = ctx
            .region
            .geocoding
            .as_ref()
            .and_then(|g| g.country_codes.split(',').next())
            .unwrap_or("")
            .trim();
        let sql = civici_sql(&release, &ext_dir, &bbox, country, &out);

        tracing::info!(release, bbox = %bbox_str, "extracting civici from Overture via DuckDB");
        let status = tokio::process::Command::new("duckdb")
            .arg("-no-stdin")
            .arg("-c")
            .arg(&sql)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "duckdb exited with {status}");
        anyhow::ensure!(
            out.is_file(),
            "duckdb reported success but civici.jsonl is absent"
        );
        Ok(())
    }
}

/// The DuckDB script that reads Overture addresses by bbox and writes
/// header-less Photon "house" docs as NDJSON. Pure, so the query shape is
/// unit-tested even though DuckDB runs in the build image.
///
/// Each emitted line is `{"type":"Place","content":[{…}]}` per the Photon JSON
/// dump format: `centroid` is `[lon,lat]`, `place_id` is the Overture id
/// sanitized to `[A-Za-z0-9_-]` and capped at 60 chars, and rows are deduped on
/// `(street, number, city)`.
fn civici_sql(release: &str, ext_dir: &str, bbox: &BBox, country: &str, out: &Path) -> String {
    let src =
        format!("s3://overturemaps-us-west-2/release/{release}/theme=addresses/type=address/*");
    format!(
        "SET extension_directory='{ext_dir}';
LOAD spatial;
LOAD httpfs;
SET s3_region='us-west-2';
COPY (
  SELECT 'Place' AS type,
         json_array(json_object(
           'place_id', left('ov' || regexp_replace(id, '[^A-Za-z0-9_-]', '', 'g'), 60),
           'object_type', 'itermaps:civico',
           'osm_key', 'place',
           'osm_value', 'house',
           'address_type', 'house',
           'importance', 0.00005,
           'housenumber', number,
           'postcode', postcode,
           'country_code', '{country}',
           'address', json_object('street', street, 'city', postal_city),
           'centroid', json_array(ST_X(geometry), ST_Y(geometry))
         )) AS content
  FROM read_parquet('{src}', hive_partitioning=1)
  WHERE number IS NOT NULL AND street IS NOT NULL
    AND bbox.xmin BETWEEN {min_lon} AND {max_lon}
    AND bbox.ymin BETWEEN {min_lat} AND {max_lat}
  QUALIFY row_number() OVER (
            PARTITION BY lower(street), number, lower(coalesce(postal_city, ''))
          ) = 1
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
    fn sql_targets_overture_addresses_and_emits_house_docs() {
        let bbox = BBox::parse("12.10,41.60,12.95,42.20").unwrap();
        let sql = civici_sql(
            "2026-06-17.0",
            "/opt/duckdb/ext",
            &bbox,
            "it",
            Path::new("/data/photon/civici.jsonl"),
        );
        // reads the right theme from the public bucket, keyless.
        assert!(sql.contains("theme=addresses/type=address"));
        assert!(sql.contains("release/2026-06-17.0/"));
        assert!(sql.contains("s3_region='us-west-2'"));
        // emits the Photon dump wrapper + house-doc fields.
        assert!(sql.contains("'Place' AS type"));
        assert!(sql.contains("'object_type', 'itermaps:civico'"));
        assert!(sql.contains("'country_code', 'it'"));
        assert!(sql.contains("'osm_value', 'house'"));
        assert!(sql.contains("'importance', 0.00005"));
        assert!(sql.contains("json_array(ST_X(geometry), ST_Y(geometry))"));
        // bbox pushdown on the struct columns.
        assert!(sql.contains("bbox.xmin BETWEEN 12.1 AND 12.95"));
        assert!(sql.contains("bbox.ymin BETWEEN 41.6 AND 42.2"));
        // dedup + headerless NDJSON output.
        assert!(sql.contains("row_number() OVER"));
        assert!(sql.contains("(FORMAT JSON, ARRAY false)"));
        assert!(sql.contains("/data/photon/civici.jsonl"));
    }

    #[test]
    fn place_id_is_sanitized_and_capped() {
        let bbox = BBox::parse("12.10,41.60,12.95,42.20").unwrap();
        let sql = civici_sql("r", "/x", &bbox, "it", Path::new("/o.jsonl"));
        // 'ov' prefix, strip non-id chars, cap at 60 (Photon place_id limit).
        assert!(sql.contains("left('ov' || regexp_replace(id, '[^A-Za-z0-9_-]', '', 'g'), 60)"));
    }
}
