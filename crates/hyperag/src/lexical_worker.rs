use crate::lexical_consumer::Consumer;
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

pub(super) struct Worker {
    cancelled: Arc<AtomicBool>,
    stop: oneshot::Sender<()>,
    done: oneshot::Receiver<()>,
    deadline: Duration,
}

impl Worker {
    pub fn start(consumer: Consumer, pool: PgPool) -> Self {
        Self::start_with(consumer, pool, Duration::from_secs(5), None)
    }

    pub(super) fn start_with(
        mut consumer: Consumer,
        pool: PgPool,
        deadline: Duration,
        after_step: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let (stop, mut stopped) = oneshot::channel();
        let (finished, done) = oneshot::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("lexical writer runtime");
            runtime.block_on(async move {
                while !flag.load(Ordering::Acquire) {
                    let step = consumer.step(&pool).await;
                    if let Some(hook) = &after_step { hook(); }
                    if flag.load(Ordering::Acquire) { break; }
                    if !matches!(step, Ok(true)) {
                        if let Err(error) = step { tracing::warn!(%error, "lexical index update failed; retaining SQL fallback"); }
                        tokio::select! { biased; _ = &mut stopped => break, _ = tokio::time::sleep(Duration::from_secs(1)) => () }
                    }
                }
                drop(consumer);
                let _ = finished.send(());
            });
        });
        Self { cancelled, stop, done, deadline }
    }

    pub async fn close(self) -> anyhow::Result<()> {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.stop.send(());
        if tokio::time::timeout(self.deadline, self.done).await.is_err() {
            tracing::warn!("lexical writer shutdown exceeded deadline; proceeding with pool close");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "lexical_worker_tests.rs"]
mod tests;
