//! Basemap tile serving: the PMTiles v3 archive over HTTP byte-range. Two hard
//! requirements survive any rebuild — the archive must be **clustered** (so the
//! offline range-extract works) and served with **gzip OFF** (tiles are already
//! internally compressed). `ServeDir` gives correct RFC 7233 range handling
//! (`Accept-Ranges`, 206) and does no compression.

use axum::Router;
use axum::http::{HeaderValue, header};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::state::AppState;

pub fn router(state: &AppState) -> Router<AppState> {
    // ServeDir does no compression and won't serve precompressed siblings
    // unless told to — so the gzip-OFF requirement holds.
    let serve = ServeDir::new(&state.cfg.tiles_dir).append_index_html_on_directories(false);

    Router::new()
        .nest_service("/tiles", serve)
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=86400, immutable"),
        ))
}
