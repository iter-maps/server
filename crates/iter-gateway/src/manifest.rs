//! Freshness manifest (`GET /manifest`): per-artifact `{updatedAt, etag}` so a
//! cache-first client checks staleness in one request instead of revalidating
//! every surface.

use std::path::Path;

use axum::Json;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use iter_contracts::health::{ArtifactFreshness, FreshnessManifest};

use crate::state::AppState;

/// Contract version the client compares against; bumped on a breaking wire change.
pub const API_VERSION: &str = "1";

pub async fn manifest(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = &state.cfg;
    let mut artifacts = std::collections::BTreeMap::new();
    artifacts.insert(
        "tiles".to_string(),
        freshness(&cfg.tiles_dir.join(&cfg.tiles_basename)).await,
    );
    artifacts.insert("styles".to_string(), freshness(&cfg.styles_dir).await);
    artifacts.insert("glyphs".to_string(), freshness(&cfg.glyphs_dir).await);
    artifacts.insert("sprite".to_string(), freshness(&cfg.sprite_dir).await);
    artifacts.insert("overlays".to_string(), freshness(&cfg.overlays_dir).await);

    let body = FreshnessManifest {
        api_version: API_VERSION.to_string(),
        generated_at: jiff::Timestamp::now().to_string(),
        artifacts,
    };

    ([(header::CACHE_CONTROL, "public, max-age=60")], Json(body))
}

/// Stat one artifact (file or dir) into a freshness record. A missing artifact
/// yields an empty record (both fields omitted) rather than an error.
async fn freshness(path: &Path) -> ArtifactFreshness {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return ArtifactFreshness::default();
    };
    let Ok(modified) = meta.modified() else {
        return ArtifactFreshness::default();
    };
    let updated_at = jiff::Timestamp::try_from(modified)
        .ok()
        .map(|t| t.to_string());
    let etag = jiff::Timestamp::try_from(modified)
        .ok()
        .map(|t| format!("W/\"{}-{}\"", meta.len(), t.as_second()));
    ArtifactFreshness { updated_at, etag }
}
