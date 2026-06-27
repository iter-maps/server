//! The step runner. Per step: skip if `SKIP_<name>`; otherwise run when forced
//! or when its output is absent, else skip as already-satisfied. Failures abort
//! the whole pipeline loudly (strict error propagation, P6) — a half-built
//! pipeline must never exit 0 and look healthy.

use crate::context::Context;
use crate::step::Step;

pub async fn run_all(ctx: &Context, steps: &[Box<dyn Step>]) -> anyhow::Result<()> {
    for step in steps {
        let name = step.name();

        if ctx.skipped(name) {
            tracing::info!(step = name, "skip (SKIP_ set)");
            continue;
        }

        let forced = ctx.forced(name);
        if !forced && step.satisfied(ctx).await {
            tracing::info!(step = name, "skip (output present)");
            continue;
        }

        tracing::info!(step = name, forced, "running");
        if let Err(e) = step.run(ctx).await {
            tracing::error!(step = name, error = %e, "step failed; aborting");
            return Err(e.context(format!("pipeline step {name} failed")));
        }
        tracing::info!(step = name, "done");
    }
    Ok(())
}
