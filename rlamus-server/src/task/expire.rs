use std::{collections::BTreeMap, fmt::Display, sync::Arc, time::Duration};

use futures::{StreamExt, TryStreamExt as _};
use thiserror::Error;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

use crate::task::TaskRegistry;

pub struct Expire<R: TaskRegistry> {
    registry: Arc<R>,
    timeout: Duration,
    timers: BTreeMap<Uuid, AbortOnDropHandle<Result<(), R::Error>>>,
}

impl<R> Expire<R>
where
    R: TaskRegistry + Sync + 'static,
    R::Error: Send + 'static,
{
    pub fn new(registry: Arc<R>, timeout: Duration) -> Self {
        Self {
            registry,
            timeout,
            timers: Default::default(),
        }
    }

    pub async fn stop(&mut self) {
        while let Some((_, handle)) = self.timers.pop_first() {
            handle.abort();
        }
    }

    pub async fn insert(&mut self, task_id: Uuid) -> InsertResult {
        let timeout = self.timeout;
        let registry = Arc::clone(&self.registry);
        let existing = self.timers.insert(
            task_id.clone(),
            AbortOnDropHandle::new(tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                registry.remove(&task_id).await?;
                Ok(())
            })),
        );
        if let Some(handle) = existing {
            handle.abort();
            InsertResult::CanceledExisting
        } else {
            InsertResult::Created
        }
    }

    pub async fn remove(&mut self, task_id: &Uuid) -> RemoveResult {
        let existing = self.timers.remove(task_id);
        if let Some(handle) = existing {
            handle.abort();
            RemoveResult::CanceledExisting
        } else {
            RemoveResult::Noop
        }
    }
}

impl<R> Expire<R>
where
    R: TaskRegistry + Sync + 'static,
    R::Error: Send + Display + WithTaskId + 'static,
{
    pub async fn start(&mut self) -> Result<(), Error<R::Error>> {
        if !self.timers.is_empty() {
            return Err(Error::NonEmptyTimers);
        }
        let new_timers = self
            .registry
            .iter()
            .map_ok(|task| {
                let task_id = task.id.clone();
                let registry = Arc::clone(&self.registry);
                let timeout = self.timeout;
                (
                    task.id,
                    AbortOnDropHandle::new(tokio::spawn(async move {
                        tokio::time::sleep(timeout).await;
                        registry.remove(&task_id).await?;
                        return Ok(());
                    })),
                )
            })
            .collect::<Vec<_>>()
            .await;
        for new_timer in new_timers {
            match new_timer {
                Ok((id, task)) => {
                    self.timers.insert(id, task);
                }
                Err(err) => {
                    if let Some(task_id) = err.task_id() {
                        tracing::warn!("{err}; expiring {task_id} immediately");
                        self.registry.remove(task_id).await?;
                    } else {
                        tracing::warn!("{err}");
                    }
                }
            }
        }
        Ok(())
    }
}

pub trait WithTaskId {
    fn task_id(&self) -> Option<&Uuid>;
}

#[derive(Debug, Error)]
pub enum Error<I> {
    #[error("registry error: {0}")]
    Registry(#[from] I),
    #[error("non-empty timers")]
    NonEmptyTimers,
}

pub enum InsertResult {
    CanceledExisting,
    Created,
}

pub enum RemoveResult {
    CanceledExisting,
    Noop,
}
