use std::{
    fmt::{Debug, Display},
    sync::Arc,
    time::Duration,
};

use crate::{
    AppState,
    task::{CachedRegistry, TaskRegistry},
};

trait Shutdown {
    async fn shutdown(&self);
}

pub async fn shutdown_handler(state: impl Shutdown) {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutting down gracefully");
    state.shutdown().await;
}

impl<R> Shutdown for Arc<AppState<CachedRegistry<R>>>
where
    R: TaskRegistry + Send + Sync,
    R::Error: Display + Debug + Send + Sync,
{
    async fn shutdown(&self) {
        match tokio::time::timeout(Duration::from_secs(5), self.registry.flush()).await {
            Ok(_) => {}
            Err(_) => {
                tracing::warn!("failed to flush registry in time, skipping");
            }
        }
        self.expire.lock().await.stop().await;
    }
}
