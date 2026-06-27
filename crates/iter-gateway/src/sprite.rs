//! Road-shield sprite sheet (`/sprite/sprite{,@2x}.{json,png}`). Static assets;
//! `ServeDir` handles content-type by extension.

use axum::Router;
use axum::http::{HeaderValue, header};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::state::AppState;

pub fn router(state: &AppState) -> Router<AppState> {
    let serve = ServeDir::new(&state.cfg.sprite_dir).append_index_html_on_directories(false);

    Router::new()
        .nest_service("/sprite", serve)
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=86400"),
        ))
}
