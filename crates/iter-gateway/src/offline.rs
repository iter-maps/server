//! Offline map pre-download. `/offline/extract` range-reads the clustered
//! basemap PMTiles into a bbox-clipped archive (no tile re-render) via the
//! pinned `go-pmtiles` CLI (concept ADR 0010). Because the surface is public
//! and auth-less, the validation caps below — bbox sanity, a maximum area, a
//! zoom clamp, and a concurrency gate — are the only abuse protection.

use std::io::Write;
use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use iter_contracts::geo::{BBox, BBoxError};
use iter_contracts::offline::{Manifest, STYLE_WHITELIST, code};
use iter_core::ApiError;
use serde::Deserialize;

use crate::config::{GatewayConfig, OfflineCaps};
use crate::http::{ApiErr, ApiResult};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtractParams {
    pub bbox: BBox,
    pub minzoom: u8,
    pub maxzoom: u8,
}

#[derive(Deserialize)]
pub struct ExtractQuery {
    bbox: Option<String>,
    minzoom: Option<u8>,
    maxzoom: Option<u8>,
}

/// Validate and normalize the request against the caps. Pure — the unit tests
/// exercise every rejection path; the handler adds the concurrency gate and the
/// CLI shell-out.
pub fn validate(
    bbox: Option<&str>,
    minzoom: Option<u8>,
    maxzoom: Option<u8>,
    caps: &OfflineCaps,
) -> Result<ExtractParams, ApiError> {
    let raw = bbox
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::new(400, code::BBOX_REQUIRED, "bbox is required"))?;

    let bbox = BBox::parse(raw).map_err(|e| {
        let (c, msg) = match e {
            BBoxError::Invalid => (
                code::BBOX_INVALID,
                "bbox must be minLon,minLat,maxLon,maxLat",
            ),
            BBoxError::OutOfRange => (code::BBOX_OUT_OF_RANGE, "bbox coordinates out of range"),
            BBoxError::Degenerate => (code::BBOX_DEGENERATE, "bbox min must be below max"),
        };
        ApiError::new(400, c, msg)
    })?;

    if bbox.area_deg2() > caps.max_area_deg2 {
        return Err(ApiError::new(
            413,
            code::AREA_TOO_LARGE,
            format!(
                "bbox area {:.2} deg^2 exceeds cap {}",
                bbox.area_deg2(),
                caps.max_area_deg2
            ),
        ));
    }

    // Zoom is silently clamped (not rejected): maxzoom to the cap, minzoom into
    // [0, maxzoom].
    let maxzoom = maxzoom.unwrap_or(caps.max_zoom).min(caps.max_zoom);
    let minzoom = minzoom.unwrap_or(0).min(maxzoom);

    Ok(ExtractParams {
        bbox,
        minzoom,
        maxzoom,
    })
}

pub async fn extract(
    State(state): State<AppState>,
    Query(query): Query<ExtractQuery>,
) -> ApiResult<Response> {
    let params = validate(
        query.bbox.as_deref(),
        query.minzoom,
        query.maxzoom,
        &state.cfg.offline,
    )
    .map_err(ApiErr)?;

    // Concurrency gate, no queue.
    let _permit = state
        .offline_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::new(503, code::BUSY, "too many concurrent extracts"))?;

    let bytes = run_extract(&state.cfg, &params).await?;
    let filename = format!("iter-offline-z{}.pmtiles", params.maxzoom);

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn run_extract(cfg: &GatewayConfig, params: &ExtractParams) -> Result<Vec<u8>, ApiErr> {
    let dir = tempfile::tempdir().map_err(|_| extract_failed())?;
    let out = dir.path().join("area.pmtiles");
    let b = &params.bbox;

    let output = tokio::process::Command::new(&cfg.pmtiles_bin)
        .arg("extract")
        .arg(&cfg.offline_source)
        .arg(&out)
        .arg(format!(
            "--bbox={},{},{},{}",
            b.min_lon, b.min_lat, b.max_lon, b.max_lat
        ))
        .arg(format!("--maxzoom={}", params.maxzoom))
        .output()
        .await
        .map_err(|_| extract_failed())?;

    if !output.status.success() {
        return Err(extract_failed().into());
    }
    let bytes = tokio::fs::read(&out).await.map_err(|_| extract_failed())?;
    Ok(bytes)
}

fn extract_failed() -> ApiError {
    ApiError::new(500, code::EXTRACT_FAILED, "PMTiles extract failed")
}

#[derive(Deserialize)]
pub struct BundleQuery {
    bbox: Option<String>,
    minzoom: Option<u8>,
    maxzoom: Option<u8>,
    styles: Option<String>,
    glyphs: Option<bool>,
    sprite: Option<bool>,
    overlays: Option<bool>,
}

pub struct BundleOptions {
    pub styles: Vec<String>,
    pub glyphs: bool,
    pub sprite: bool,
    pub overlays: bool,
}

struct BundleDirs {
    styles: PathBuf,
    glyphs: PathBuf,
    sprite: PathBuf,
    overlays: PathBuf,
    /// The served basemap filename (`rome.pmtiles`), so the style rewrite points
    /// the right source at the bundled `area.pmtiles`.
    tiles_basename: String,
}

/// `GET /offline/bundle`: an extract plus the styles (rewritten to point at the
/// bundled `area.pmtiles`), glyphs, sprite, overlays, and a `manifest.json`,
/// assembled into a STORE zip. The literal `__BASE_URL__` is kept so the client
/// rewrites it to `file://` at unpack time.
pub async fn bundle(
    State(state): State<AppState>,
    Query(query): Query<BundleQuery>,
) -> ApiResult<Response> {
    let params = validate(
        query.bbox.as_deref(),
        query.minzoom,
        query.maxzoom,
        &state.cfg.offline,
    )
    .map_err(ApiErr)?;
    let opts = bundle_options(
        query.styles.as_deref(),
        query.glyphs,
        query.sprite,
        query.overlays,
    );

    let _permit = state
        .offline_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::new(503, code::BUSY, "too many concurrent extracts"))?;

    let area = run_extract(&state.cfg, &params).await?;

    let dirs = BundleDirs {
        styles: state.cfg.styles_dir.clone(),
        glyphs: state.cfg.glyphs_dir.clone(),
        sprite: state.cfg.sprite_dir.clone(),
        overlays: state.cfg.overlays_dir.clone(),
        tiles_basename: state.cfg.tiles_basename.clone(),
    };
    let generator = format!("iter-gateway/{}", state.cfg.version);

    let zip =
        tokio::task::spawn_blocking(move || build_bundle(&dirs, &generator, &params, &opts, area))
            .await
            .map_err(|_| ApiErr(extract_failed()))?
            .map_err(|_| ApiErr(extract_failed()))?;

    let filename = format!("iter-offline-bundle-z{}.zip", params.maxzoom);
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        zip,
    )
        .into_response())
}

fn bundle_options(
    styles_csv: Option<&str>,
    glyphs: Option<bool>,
    sprite: Option<bool>,
    overlays: Option<bool>,
) -> BundleOptions {
    let styles = match styles_csv {
        Some(csv) => csv
            .split(',')
            .map(str::trim)
            .filter(|s| STYLE_WHITELIST.contains(s))
            .map(str::to_string)
            .collect(),
        None => STYLE_WHITELIST.iter().map(|s| s.to_string()).collect(),
    };
    BundleOptions {
        styles,
        glyphs: glyphs.unwrap_or(true),
        sprite: sprite.unwrap_or(true),
        overlays: overlays.unwrap_or(true),
    }
}

fn build_bundle(
    dirs: &BundleDirs,
    generator: &str,
    params: &ExtractParams,
    opts: &BundleOptions,
    area: Vec<u8>,
) -> std::io::Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let file_opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("area.pmtiles", file_opts)?;
    zip.write_all(&area)?;

    let mut included_styles = Vec::new();
    for name in &opts.styles {
        let path = dirs.styles.join(format!("{name}.json"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            zip.start_file(format!("styles/{name}.json"), file_opts)?;
            zip.write_all(rewrite_style_tiles(&text, &dirs.tiles_basename).as_bytes())?;
            included_styles.push(name.clone());
        }
    }

    if opts.glyphs {
        add_dir(&mut zip, file_opts, &dirs.glyphs, "glyphs")?;
    }
    if opts.sprite {
        add_dir(&mut zip, file_opts, &dirs.sprite, "sprite")?;
    }

    let mut overlay_files = Vec::new();
    if opts.overlays && dirs.overlays.is_dir() {
        for entry in std::fs::read_dir(&dirs.overlays)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("geojson") {
                let name = entry.file_name().to_string_lossy().into_owned();
                zip.start_file(format!("overlays/{name}"), file_opts)?;
                zip.write_all(&std::fs::read(&path)?)?;
                overlay_files.push(name);
            }
        }
    }

    let b = &params.bbox;
    let manifest = Manifest {
        generator: generator.to_string(),
        created_at: jiff::Timestamp::now().to_string(),
        bbox: [b.min_lon, b.min_lat, b.max_lon, b.max_lat],
        minzoom: params.minzoom,
        maxzoom: params.maxzoom,
        pmtiles: "area.pmtiles".to_string(),
        styles: included_styles,
        glyphs: opts.glyphs,
        sprite: opts.sprite,
        overlays: overlay_files,
        note: "Rewrite the literal __BASE_URL__ to file://<unpack-dir> when rendering offline."
            .to_string(),
    };
    zip.start_file("manifest.json", file_opts)?;
    let json = serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?;
    zip.write_all(&json)?;

    Ok(zip.finish()?.into_inner())
}

/// Point the style's tile source at the bundled archive; keep `__BASE_URL__`
/// literal for the client's `file://` substitution.
fn rewrite_style_tiles(style: &str, tiles_basename: &str) -> String {
    style.replace(&format!("/tiles/{tiles_basename}"), "/area.pmtiles")
}

fn add_dir<W: std::io::Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    opts: zip::write::SimpleFileOptions,
    dir: &Path,
    prefix: &str,
) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let zip_path = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if path.is_dir() {
            add_dir(zip, opts, &path, &zip_path)?;
        } else {
            zip.start_file(&zip_path, opts)?;
            zip.write_all(&std::fs::read(&path)?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> OfflineCaps {
        OfflineCaps {
            max_area_deg2: 6.0,
            max_zoom: 14,
            max_concurrent: 3,
        }
    }

    #[test]
    fn missing_bbox_is_required() {
        let err = validate(None, None, None, &caps()).unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.code, code::BBOX_REQUIRED);
        let err = validate(Some("  "), None, None, &caps()).unwrap_err();
        assert_eq!(err.code, code::BBOX_REQUIRED);
    }

    #[test]
    fn bbox_error_codes() {
        assert_eq!(
            validate(Some("1,2,3"), None, None, &caps())
                .unwrap_err()
                .code,
            code::BBOX_INVALID
        );
        assert_eq!(
            validate(Some("0,0,200,1"), None, None, &caps())
                .unwrap_err()
                .code,
            code::BBOX_OUT_OF_RANGE
        );
        assert_eq!(
            validate(Some("5,5,5,6"), None, None, &caps())
                .unwrap_err()
                .code,
            code::BBOX_DEGENERATE
        );
    }

    #[test]
    fn area_cap_rejects_oversized() {
        // 10x10 deg = 100 deg^2 > 6.
        let err = validate(Some("0,0,10,10"), None, None, &caps()).unwrap_err();
        assert_eq!(err.status, 413);
        assert_eq!(err.code, code::AREA_TOO_LARGE);
    }

    #[test]
    fn zoom_is_clamped_not_rejected() {
        let p = validate(Some("12.4,41.8,12.6,42.0"), Some(99), Some(99), &caps()).unwrap();
        assert_eq!(p.maxzoom, 14, "maxzoom clamped to the cap");
        assert_eq!(p.minzoom, 14, "minzoom clamped into [0, maxzoom]");

        let p = validate(Some("12.4,41.8,12.6,42.0"), None, None, &caps()).unwrap();
        assert_eq!(p.maxzoom, 14);
        assert_eq!(p.minzoom, 0);
    }

    #[test]
    fn accepts_small_bbox() {
        let p = validate(Some("12.4,41.8,12.6,42.0"), Some(5), Some(12), &caps()).unwrap();
        assert_eq!(p.minzoom, 5);
        assert_eq!(p.maxzoom, 12);
        assert!((p.bbox.min_lon - 12.4).abs() < 1e-9);
    }

    #[test]
    fn bundle_options_default_and_whitelist() {
        let o = bundle_options(None, None, None, None);
        assert_eq!(o.styles.len(), 4);
        assert!(o.glyphs && o.sprite && o.overlays);

        let o = bundle_options(Some("light,bogus,dark"), Some(false), None, None);
        assert_eq!(
            o.styles,
            vec!["light", "dark"],
            "bogus dropped by whitelist"
        );
        assert!(!o.glyphs);
    }

    #[test]
    fn build_bundle_assembles_zip_with_rewritten_styles() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("styles")).unwrap();
        std::fs::write(
            root.join("styles/light.json"),
            r#"{"sources":{"b":{"url":"pmtiles://__BASE_URL__/tiles/rome.pmtiles"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("overlays")).unwrap();
        std::fs::write(root.join("overlays/metro-stations.geojson"), b"{}").unwrap();

        let dirs = BundleDirs {
            styles: root.join("styles"),
            glyphs: root.join("glyphs"), // absent → skipped, not an error
            sprite: root.join("sprite"), // absent → skipped
            overlays: root.join("overlays"),
            tiles_basename: "rome.pmtiles".to_string(),
        };
        let params = ExtractParams {
            bbox: BBox::parse("12,41,13,42").unwrap(),
            minzoom: 0,
            maxzoom: 14,
        };
        let opts = BundleOptions {
            styles: vec!["light".to_string()],
            glyphs: true,
            sprite: true,
            overlays: true,
        };

        let bytes = build_bundle(&dirs, "iter-test", &params, &opts, b"PMTILES".to_vec()).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();

        let names: std::collections::HashSet<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains("area.pmtiles"));
        assert!(names.contains("styles/light.json"));
        assert!(names.contains("overlays/metro-stations.geojson"));
        assert!(names.contains("manifest.json"));

        let mut style = String::new();
        archive
            .by_name("styles/light.json")
            .unwrap()
            .read_to_string(&mut style)
            .unwrap();
        assert!(style.contains("/area.pmtiles"));
        assert!(!style.contains("/tiles/rome.pmtiles"));
        assert!(
            style.contains("__BASE_URL__"),
            "the placeholder is kept for the client"
        );

        let mut manifest = String::new();
        archive
            .by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(v["pmtiles"], "area.pmtiles");
        assert_eq!(v["styles"], serde_json::json!(["light"]));
        assert_eq!(v["overlays"], serde_json::json!(["metro-stations.geojson"]));
    }
}
