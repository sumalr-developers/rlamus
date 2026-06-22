use std::error::Error as StdError;

use futures::{TryFutureExt, future::BoxFuture};
use thiserror::Error;
use tracing::{Instrument, debug_span};
use url::Url;

use crate::scraper::reddit::RedditSiteScraper;

trait SiteScraperHolder {
    fn can_handle(&self, url: &Url) -> bool;
    fn scrape_markdown<'a>(
        &'a self,
        url: &'a Url,
    ) -> BoxFuture<'a, Result<String, Box<dyn StdError>>>;
}

struct ScraperInfo {
    name: &'static str,
}

pub trait SiteScraper: Send {
    type Error: StdError;

    fn name() -> &'static str;

    fn can_handle(&self, url: &Url) -> bool;
    fn scrape_markdown(
        &self,
        url: &Url,
    ) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

pub struct CompatibilityLayer {
    scrapers: Vec<(ScraperInfo, Box<dyn SiteScraperHolder>)>,
}

impl CompatibilityLayer {
    pub fn new() -> Self {
        Self {
            scrapers: Default::default(),
        }
    }

    pub fn with_site_scraper<S>(mut self, scraper: S) -> Self
    where
        S: SiteScraper + 'static,
    {
        self.scrapers
            .push((ScraperInfo::new::<S>(), Box::new(scraper)));
        self
    }

    pub async fn scrape_markdown(&self, url: &str) -> Result<String, Error> {
        let url = Url::parse(url)?;
        let (info, scraper) = self
            .scrapers
            .iter()
            .find(|(info, it)| {
                debug_span!("can_handle", scraper = info.name).in_scope(|| it.can_handle(&url))
            })
            .ok_or(Error::CannotHandle)?;
        Ok(scraper
            .scrape_markdown(&url)
            .instrument(debug_span!("scrape_markdown", scraper = info.name))
            .await?)
    }
}

impl Default for CompatibilityLayer {
    fn default() -> Self {
        Self {
            scrapers: vec![(
                ScraperInfo::new::<RedditSiteScraper>(),
                Box::new(RedditSiteScraper::default()),
            )],
        }
    }
}

impl<S> SiteScraperHolder for S
where
    S: SiteScraper,
    S::Error: StdError + 'static,
{
    fn can_handle(&self, url: &Url) -> bool {
        self.can_handle(url)
    }

    fn scrape_markdown<'a>(
        &'a self,
        url: &'a Url,
    ) -> BoxFuture<'a, Result<String, Box<dyn StdError>>> {
        Box::pin(
            self.scrape_markdown(url)
                .map_err(|err| Box::new(err) as Box<dyn StdError>),
        )
    }
}

impl ScraperInfo {
    fn new<S>() -> Self
    where
        S: SiteScraper,
    {
        ScraperInfo { name: S::name() }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("parse: {0}")]
    ParseError(#[from] url::ParseError),
    #[error("cannot handle")]
    CannotHandle,
    #[error("scrap: {0}")]
    ScrapError(#[from] Box<dyn StdError>),
}
