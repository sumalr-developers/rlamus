use std::{collections::BTreeMap, sync::Arc};

use rlamus_core::{scraper::Scraper, summarize::Summarize};
use tokio::sync::{Mutex, RwLock};
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

use crate::task::{TaskRegistry, expire::Expire};

/// Stateful
pub struct AppState<R: TaskRegistry> {
    pub registry: Arc<R>,
    pub handles: RwLock<BTreeMap<Uuid, AbortOnDropHandle<()>>>,
    pub expire: Mutex<Expire<R>>,
}

/// Stateless
pub struct AppFoundation {
    pub scraper: Scraper,
    pub summarizer: Summarize,
    pub apn_client: Option<apns_h2::Client>,
}
