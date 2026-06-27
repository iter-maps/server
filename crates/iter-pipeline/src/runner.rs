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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::step::Step;

    struct FakeStep {
        name: &'static str,
        satisfied: bool,
        fail: bool,
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Step for FakeStep {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn satisfied(&self, _ctx: &Context) -> bool {
            self.satisfied
        }
        async fn run(&self, _ctx: &Context) -> anyhow::Result<()> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("boom");
            }
            Ok(())
        }
    }

    fn ctx() -> Context {
        Context {
            data_dir: std::path::PathBuf::from("/tmp"),
            version: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn runs_unsatisfied_and_skips_satisfied() {
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let steps: Vec<Box<dyn Step>> = vec![
            Box::new(FakeStep {
                name: "STEP_A",
                satisfied: false,
                fail: false,
                ran: a.clone(),
            }),
            Box::new(FakeStep {
                name: "STEP_B",
                satisfied: true,
                fail: false,
                ran: b.clone(),
            }),
        ];
        run_all(&ctx(), &steps).await.unwrap();
        assert_eq!(a.load(Ordering::SeqCst), 1, "unsatisfied step runs");
        assert_eq!(b.load(Ordering::SeqCst), 0, "satisfied step is skipped");
    }

    #[tokio::test]
    async fn aborts_on_first_failure() {
        let first = Arc::new(AtomicUsize::new(0));
        let after = Arc::new(AtomicUsize::new(0));
        let steps: Vec<Box<dyn Step>> = vec![
            Box::new(FakeStep {
                name: "STEP_FAIL",
                satisfied: false,
                fail: true,
                ran: first.clone(),
            }),
            Box::new(FakeStep {
                name: "STEP_AFTER",
                satisfied: false,
                fail: false,
                ran: after.clone(),
            }),
        ];
        let result = run_all(&ctx(), &steps).await;
        assert!(result.is_err());
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(
            after.load(Ordering::SeqCst),
            0,
            "steps after a failure do not run"
        );
    }
}
