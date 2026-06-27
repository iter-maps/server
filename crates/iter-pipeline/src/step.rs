use async_trait::async_trait;

use crate::context::Context;

/// One idempotent pipeline step. A step declares whether its output already
/// exists (`satisfied`) so re-runs finish in seconds, and does its work in
/// `run`. Steps must write outputs atomically (temp + rename) so a crash never
/// leaves a half-written artifact that looks done.
#[async_trait]
pub trait Step: Send + Sync {
    fn name(&self) -> &'static str;

    /// True when the step's output already exists and is current.
    async fn satisfied(&self, ctx: &Context) -> bool;

    async fn run(&self, ctx: &Context) -> anyhow::Result<()>;
}
