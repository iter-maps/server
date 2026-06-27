use std::net::SocketAddr;
use std::path::PathBuf;

use iter_core::config;

/// Gateway configuration, entirely env-derived (`.env` for "clone + up").
/// Fields are added as the capabilities that consume them land.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub bind: SocketAddr,
    /// Root of the read-only artifact tree the pipeline produces.
    pub data_dir: PathBuf,
}

impl GatewayConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let host = config::or("GATEWAY_HOST", "0.0.0.0");
        let port: u16 = config::parse("GATEWAY_PORT", 8090);
        Ok(Self {
            bind: format!("{host}:{port}").parse()?,
            data_dir: PathBuf::from(config::or("DATA_DIR", "/data")),
        })
    }
}
