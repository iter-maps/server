use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use iter_core::config;

/// Gateway configuration, entirely env-derived (`.env` for "clone + up").
/// Fields are added as the capabilities that consume them land.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub bind: SocketAddr,
    /// Root of the read-only artifact tree the pipeline produces.
    pub data_dir: PathBuf,
    pub tiles_dir: PathBuf,
    pub styles_dir: PathBuf,
    pub glyphs_dir: PathBuf,
    pub sprite_dir: PathBuf,
    pub overlays_dir: PathBuf,
}

impl GatewayConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let host = config::or("GATEWAY_HOST", "0.0.0.0");
        let port: u16 = config::parse("GATEWAY_PORT", 8090);
        let data_dir = PathBuf::from(config::or("DATA_DIR", "/data"));
        Ok(Self {
            bind: format!("{host}:{port}").parse()?,
            tiles_dir: dir("TILES_DIR", &data_dir, "output/tiles"),
            styles_dir: dir("STYLES_DIR", &data_dir, "output/styles"),
            glyphs_dir: dir("GLYPHS_DIR", &data_dir, "static/glyphs"),
            sprite_dir: dir("SPRITE_DIR", &data_dir, "static/sprite"),
            overlays_dir: dir("OVERLAYS_DIR", &data_dir, "output/overlays"),
            data_dir,
        })
    }
}

/// An artifact directory: explicit env override, else derived from `data_dir`.
fn dir(env_key: &str, data_dir: &Path, rel: &str) -> PathBuf {
    config::opt(env_key).map(PathBuf::from).unwrap_or_else(|| data_dir.join(rel))
}
