use std::path::PathBuf;

use iter_core::config;

/// Pipeline run context. The `FORCE_*`/`SKIP_*` knobs key off step names, so a
/// single thing can be refreshed (`FORCE_TILES=true`) or opted out
/// (`SKIP_PHOTON=true`) without rebuilding everything.
pub struct Context {
    pub data_dir: PathBuf,
    pub version: String,
}

impl Context {
    pub fn from_env() -> Self {
        Self {
            data_dir: PathBuf::from(config::or("DATA_DIR", "/data")),
            version: config::or("ITER_VERSION", env!("CARGO_PKG_VERSION")),
        }
    }

    /// `FORCE_<STEP>=true` (or the blanket `FORCE_ALL=true`) re-runs a step
    /// even when its output already exists.
    pub fn forced(&self, step: &str) -> bool {
        config::flag("FORCE_ALL", false) || config::flag(&format!("FORCE_{step}"), false)
    }

    /// `SKIP_<STEP>=true` opts a step out entirely.
    pub fn skipped(&self, step: &str) -> bool {
        config::flag(&format!("SKIP_{step}"), false)
    }

    pub fn output(&self, rel: &str) -> PathBuf {
        self.data_dir.join(rel)
    }
}
