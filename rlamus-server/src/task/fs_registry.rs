use async_stream::{stream, try_stream};
use futures::Stream;
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;

use crate::task::{Task, TaskRegistry};

pub struct FsRegistry {
    base_dir: PathBuf,
    tx: broadcast::Sender<Task>,
    rx: broadcast::Receiver<Task>,
}

impl FsRegistry {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        let (tx, rx) = broadcast::channel(1);
        Self {
            base_dir: base_dir.into(),
            tx,
            rx,
        }
    }

    fn path_by_id(&self, id: &Uuid) -> PathBuf {
        self.base_dir.join(format!("{}.json", id))
    }
}

impl TaskRegistry for FsRegistry {
    type Error = Error;

    async fn insert(&self, task: Task) -> Result<(), Self::Error> {
        tokio::fs::write(
            self.path_by_id(&task.id),
            serde_json::to_vec(&task).unwrap(),
        )
        .await?;
        _ = self.tx.send(task);
        Ok(())
    }

    async fn remove(&self, id: &Uuid) -> Result<Option<Task>, Self::Error> {
        let Some(task) = self.get(id).await? else {
            return Ok(None);
        };
        tokio::fs::remove_file(self.path_by_id(id)).await?;
        Ok(Some(task))
    }

    async fn get(&self, id: &Uuid) -> Result<Option<Task>, Self::Error> {
        let path = self.path_by_id(&id);
        if !tokio::fs::try_exists(&path).await? {
            return Ok(None);
        }
        let buf = tokio::fs::read(&path).await?;
        Ok(Some(serde_json::from_slice(&buf)?))
    }

    fn iter(&self) -> impl Stream<Item = Result<Task, Self::Error>> {
        try_stream! {
            let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if !path.is_file() || !path.extension().is_some_and(|ext| ext == "json") {
                    continue;
                }
                let task = serde_json::from_slice(tokio::fs::read(path).await?.as_slice())?;
                yield task;
            }
        }
    }

    fn changes_on(&self, id: Uuid) -> impl Stream<Item = Task> + use<> {
        let mut rx = self.rx.resubscribe();

        stream! {
            loop {
                match rx.recv().await {
                    Ok(task) => {
                        if task.id == id {
                            yield task;
                        }
                    },
                    Err(RecvError::Lagged(_)) => {}
                    Err(_) => panic!("fs registry changes broadcast channel dropped"),
                }
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO: {0}")]
    IO(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}
