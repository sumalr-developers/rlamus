use async_stream::{stream, try_stream};
use futures::Stream;
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};
use thiserror::Error;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;

use crate::task::{Task, TaskRegistry, expire::WithTaskId};

pub struct FsRegistry {
    base_dir: PathBuf,
    tx: broadcast::Sender<(Uuid, Option<Task>)>,
    rx: broadcast::Receiver<(Uuid, Option<Task>)>,
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
        let path = self.path_by_id(&task.id);
        tokio::fs::write(&path, serde_json::to_vec(&task).unwrap())
            .await
            .map_err(|err| Error(err.into(), entry_id(&path), path))?;
        _ = self.tx.send((task.id, Some(task)));
        Ok(())
    }

    async fn remove(&self, id: &Uuid) -> Result<Option<Task>, Self::Error> {
        let path = self.path_by_id(id);
        let removed = self.get(id).await;
        if let Err(err) = tokio::fs::remove_file(&path).await
            && err.kind() == std::io::ErrorKind::NotFound
            && removed.as_ref().is_ok_and(|it| it.is_some())
        {
            return Err(Error(err.into(), entry_id(&path), path));
        }

        _ = self.tx.send((id.clone(), None));
        Ok(removed?)
    }

    async fn get(&self, id: &Uuid) -> Result<Option<Task>, Self::Error> {
        let mut path = Some(self.path_by_id(&id));
        if !tokio::fs::try_exists(path.as_ref().unwrap())
            .await
            .map_err(|err| {
                let path = path.take().unwrap();
                Error(err.into(), entry_id(&path), path)
            })?
        {
            return Ok(None);
        }
        let buf = tokio::fs::read(path.as_ref().unwrap())
            .await
            .map_err(|err| {
                let path = path.clone().take().unwrap();
                Error(err.into(), entry_id(&path), path.clone())
            })?;
        Ok(Some(serde_json::from_slice(&buf).map_err(|err| {
            let path = path.take().unwrap();
            Error(err.into(), entry_id(&path), path)
        })?))
    }

    fn iter(&self) -> impl Stream<Item = Result<Task, Self::Error>> {
        try_stream! {
            let mut entries = tokio::fs::read_dir(&self.base_dir).await.map_err(|err| Error(err.into(), None, self.base_dir.clone()))?;
            while let Some(entry) = entries.next_entry().await.map_err(|err| Error(err.into(), None, self.base_dir.clone()))? {
                let path = entry.path();
                if !path.is_file() || !path.extension().is_some_and(|ext| ext == "json") {
                    continue;
                }
                let task = serde_json::from_slice(
                    tokio::fs::read(&path)
                        .await
                        .map_err(|err| Error(err.into(), entry_id(&path), path.clone()))?
                        .as_slice()
                ).map_err(|err| Error(err.into(), entry_id(&path), path))?;
                yield task;
            }
        }
    }

    fn changes_on(&self, id: Uuid) -> impl Stream<Item = Option<Task>> + use<> {
        let mut rx = self.rx.resubscribe();

        stream! {
            loop {
                match rx.recv().await {
                    Ok((task_id, change)) => {
                        if task_id == id {
                            yield change;
                        }
                    },
                    Err(RecvError::Lagged(_)) => {}
                    Err(_) => panic!("fs registry changes broadcast channel dropped"),
                }
            }
        }
    }
}

fn entry_id(path: &Path) -> Option<Uuid> {
    path.file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| Uuid::from_str(name).ok())
}

#[derive(Debug, Error)]
#[error("{0} (id: {1:?}, file: {2})")]
pub struct Error(ErrorKind, Option<Uuid>, PathBuf);

#[derive(Debug, Error)]
pub enum ErrorKind {
    #[error("IO: {0}")]
    IO(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

impl WithTaskId for Error {
    fn task_id(&self) -> Option<&Uuid> {
        self.1.as_ref()
    }
}
