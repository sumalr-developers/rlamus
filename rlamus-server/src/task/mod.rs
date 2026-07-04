use futures::Stream;
use serde::{Deserialize, Serialize};
use smol_str::{SmolStr, ToSmolStr};
use uuid::Uuid;

mod cached_registry;
mod fs_registry;
pub mod expire;

pub use cached_registry::CachedRegistry;
pub use fs_registry::FsRegistry;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub url: SmolStr,
    pub state: TaskState,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    #[default]
    Init,
    Scraping,
    Summarizing {
        title: Option<SmolStr>,
    },
    Done {
        title: Option<SmolStr>,
        summary: SmolStr,
    },
    Failed(SmolStr),
}

#[trait_variant::make(Send)]
pub trait TaskRegistry {
    type Error;

    async fn insert(&self, task: Task) -> Result<(), Self::Error>;
    async fn remove(&self, id: &Uuid) -> Result<Option<Task>, Self::Error>;
    async fn get(&self, id: &Uuid) -> Result<Option<Task>, Self::Error>;
    fn iter(&self) -> impl Stream<Item = Result<Task, Self::Error>>;

    fn changes_on(&self, id: Uuid) -> impl Stream<Item = Task> + use<Self>;
}

impl Task {
    pub fn new(url: impl ToSmolStr) -> Self {
        Self {
            id: Uuid::new_v4(),
            url: url.to_smolstr(),
            state: TaskState::Init,
        }
    }
}
