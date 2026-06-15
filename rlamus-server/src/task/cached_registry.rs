use std::{
    collections::{HashMap, LinkedList},
    fmt::Display,
    sync::Arc,
};

use async_stream::stream;
use axum::response::IntoResponse;
use futures::{Stream, StreamExt};
use thiserror::Error;
use tokio::sync::{
    RwLock,
    broadcast::{self, error::RecvError},
};
use uuid::Uuid;

use crate::task::{Task, TaskRegistry};

pub struct CachedRegistry<Inner>
where
    Inner: TaskRegistry + Send + Sync + 'static,
    Inner::Error: Display,
{
    inner: Arc<RwLock<Inner>>,
    cache: Arc<RwLock<HashMap<Uuid, Task>>>,
    cache_limit: usize,
    op_queue: Arc<RwLock<HashMap<Uuid, Op>>>,
    auto_flush: usize,
    lru: RwLock<LinkedList<Uuid>>,
    tx: broadcast::Sender<Task>,
    rx: broadcast::Receiver<Task>,
}

#[derive(Clone, Copy)]
enum Op {
    Insert,
    Remove,
}

impl<Inner> CachedRegistry<Inner>
where
    Inner: TaskRegistry + Send + Sync + 'static,
    Inner::Error: Display,
{
    pub fn new(inner: Inner) -> Self {
        let (tx, rx) = broadcast::channel(1);
        Self {
            inner: Arc::new(RwLock::new(inner)),
            cache: Default::default(),
            cache_limit: 50,
            op_queue: Default::default(),
            auto_flush: 5,
            lru: Default::default(),
            tx,
            rx,
        }
    }

    pub fn with_cache_limit(mut self, cache_limit: usize) -> Self {
        self.cache_limit = cache_limit;
        self
    }

    pub fn with_auto_flush(mut self, auto_flush: usize) -> Self {
        self.auto_flush = auto_flush;
        self
    }

    pub fn no_auto_flush(mut self) -> Self {
        self.auto_flush = usize::MAX;
        self
    }

    pub async fn flush(&self) -> Result<(), Error<Inner::Error>> {
        let mut inner = self.inner.write().await;
        let mut op_queue = self.op_queue.write().await;
        for (id, op) in op_queue.iter() {
            match op {
                Op::Insert => {
                    inner
                        .insert(self.cache.read().await.get(id).unwrap().clone())
                        .await
                        .map_err(|err| Error {
                            id: Some(id.clone()),
                            inner: err,
                        })?;
                }
                Op::Remove => {
                    inner.remove(id).await.map_err(|err| Error {
                        id: Some(id.clone()),
                        inner: err,
                    })?;
                }
            }
        }
        op_queue.clear();
        Ok(())
    }
}

impl<Inner> CachedRegistry<Inner>
where
    Inner: TaskRegistry + Send + Sync + 'static,
    Inner::Error: Display,
{
    fn drop(&mut self) {
        let inner = Arc::clone(&self.inner);
        let op_queue = Arc::clone(&self.op_queue);
        let cache = Arc::clone(&self.cache);
        tokio::spawn(async move {
            let mut write_guard = inner.write().await;
            let mut cache = cache.write().await;
            for (id, op) in op_queue.read().await.iter() {
                if let Some(task) = cache.remove(id) {
                    let err = match op {
                        Op::Insert => write_guard.insert(task).await.err(),
                        Op::Remove => write_guard.remove(&id).await.err(),
                    };
                    if let Some(err) = err {
                        tracing::error!("error for {id}: {err}");
                    }
                }
            }
        });
    }
}

impl<Inner> TaskRegistry for CachedRegistry<Inner>
where
    Inner: TaskRegistry + Send + Sync + 'static,
    Inner::Error: Send + Sync + Display,
{
    type Error = Error<Inner::Error>;

    async fn insert(&mut self, task: Task) -> Result<(), Self::Error> {
        self.op_queue
            .write()
            .await
            .insert(task.id.clone(), Op::Insert);
        self.lru.get_mut().push_back(task.id.clone());
        let mut cache = self.cache.write().await;
        cache.insert(task.id.clone(), task.clone());

        if cache.len() > self.cache_limit || self.op_queue.read().await.len() >= self.auto_flush {
            self.flush().await?;
            while cache.len() > self.cache_limit {
                let Some(id) = self.lru.get_mut().pop_front() else {
                    break;
                };
                cache.remove(&id);
            }
        }
        _ = self.tx.send(task);
        Ok(())
    }

    async fn remove(&mut self, id: &Uuid) -> Result<Option<Task>, Self::Error> {
        self.op_queue.write().await.insert(id.clone(), Op::Remove);
        self.lru.get_mut().extract_if(|it| it == id).next();
        if self.op_queue.read().await.len() >= self.auto_flush {
            self.flush().await?;
        }

        Ok(self.cache.write().await.remove(id))
    }

    async fn get(&self, id: &Uuid) -> Result<Option<Task>, Self::Error> {
        if let Some(Op::Remove) = self.op_queue.read().await.get(id) {
            return Ok(None);
        }
        {
            let mut lru = self.lru.write().await;
            lru.extract_if(|it| it == id).next();
            lru.push_back(id.clone());
        }

        if let Some(task) = self.cache.read().await.get(id).cloned() {
            return Ok(Some(task));
        }
        let read = self.inner.read().await.get(id).await.map_err(|err| Error {
            id: Some(id.clone()),
            inner: err,
        })?;
        if let Some(task) = read.as_ref() {
            self.cache.write().await.insert(id.clone(), task.clone());
        }
        Ok(read)
    }

    fn iter(&self) -> impl Stream<Item = Result<Task, Self::Error>> {
        stream! {
            let inner = self.inner.read().await;
            let mut stream = Box::pin(inner.iter());
            while let Some(task) = stream.next().await {
                yield task.map_err(|err| Error {
                    id: None,
                    inner: err,
                });
            }
        }
    }

    fn changes_on(&mut self, id: &Uuid) -> impl Stream<Item = Task> {
        let mut rx = self.rx.resubscribe();

        stream! {
            loop {
                match rx.recv().await {
                    Ok(task) => {
                        if &task.id == id {
                            yield task;
                        }
                    },
                    Err(RecvError::Lagged(_)) => {}
                    Err(_) => panic!("cached registry changes broadcast channel dropped"),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Error)]
#[error("error for {id:?}: {inner}")]
pub struct Error<Inner> {
    id: Option<Uuid>,
    inner: Inner,
}

impl<Inner> IntoResponse for Error<Inner>
where
    Inner: Display,
{
    fn into_response(self) -> axum::response::Response {
        axum::response::Response::builder()
            .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
            .body(self.to_string().into())
            .unwrap()
    }
}
