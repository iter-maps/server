//! Runs each job on its own cadence and drains them on shutdown. A `watch`
//! channel broadcasts the shutdown so every job loop exits promptly when the
//! orchestrator sends SIGTERM.

use crate::job::Job;

pub async fn run(jobs: Vec<Box<dyn Job>>) -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let mut handles = Vec::new();
    for job in jobs {
        let mut stop = stop_rx.clone();
        handles.push(tokio::spawn(async move {
            if job.run_on_start() {
                run_once(job.as_ref()).await;
            }

            let mut ticker = tokio::time::interval(job.interval());
            ticker.tick().await; // the first tick fires immediately; skip it

            loop {
                tokio::select! {
                    _ = ticker.tick() => run_once(job.as_ref()).await,
                    _ = stop.changed() => break,
                }
            }
        }));
    }

    iter_core::shutdown::signal().await;
    let _ = stop_tx.send(true);

    for handle in handles {
        let _ = handle.await;
    }
    tracing::info!("all jobs drained");
    Ok(())
}

async fn run_once(job: &dyn Job) {
    let name = job.name();
    tracing::info!(job = name, "running");
    match job.run().await {
        Ok(()) => tracing::info!(job = name, "done"),
        Err(e) => tracing::error!(job = name, error = %e, "job failed (will retry next interval)"),
    }
}
