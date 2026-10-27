use crate::lexical_consumer::Consumer;
use sqlx::PgPool;
use std::time::Duration;
use tokio::{sync::oneshot, task::JoinHandle};

pub(super) struct Worker {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Worker {
    pub fn start(mut consumer: Consumer, pool: PgPool) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    result = consumer.step(&pool) => result,
                };
                match result {
                    Ok(true) => tokio::task::yield_now().await,
                    result => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "lexical index update failed; retaining SQL fallback");
                        }
                        tokio::select! {
                            biased;
                            _ = &mut stopped => break,
                            _ = tokio::time::sleep(Duration::from_secs(1)) => (),
                        }
                    }
                }
            }
            // Consumer and its pending snapshot drop before the join completes.
        });
        Self { stop, task }
    }

    pub async fn close(mut self) -> anyhow::Result<()> {
        let _ = self.stop.send(());
        match tokio::time::timeout(Duration::from_secs(5), &mut self.task).await {
            Ok(result) => result?,
            Err(_) => {
                self.task.abort();
                let _ = self.task.await;
                anyhow::bail!("lexical writer shutdown timed out");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "lexical_worker_tests.rs"]
mod tests;
