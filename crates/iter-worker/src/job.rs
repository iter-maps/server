use std::time::Duration;

use async_trait::async_trait;

/// A recurring background job. Jobs run optionally on startup and then every
/// `interval`. Unlike pipeline steps (which abort the build on failure), a job
/// failure is logged and the schedule continues — a transient upstream blip
/// must not take the worker down.
#[async_trait]
pub trait Job: Send + Sync {
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;

    fn run_on_start(&self) -> bool {
        true
    }

    async fn run(&self) -> anyhow::Result<()>;
}
