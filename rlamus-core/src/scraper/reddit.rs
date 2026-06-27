use std::{
    collections::{BTreeMap, HashMap},
    str::FromStr,
    time::Duration,
};

use reqwest::header::{self, HeaderMap, HeaderName};
use thiserror::Error;
use url::Url;

use crate::{
    environ,
    scraper::{ScrapeResult, compatiblity::SiteScraper, convert_html_to_md},
};

pub struct RedditSiteScraper {
    client: reqwest::Client,
}

impl RedditSiteScraper {
    fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for RedditSiteScraper {
    fn default() -> Self {
        let headers: HeaderMap = if let Some(headers_str) = std::env::var_os("REDDIT_HEADERS") {
            let headers_kv: BTreeMap<String, String> = environ::from_os_str(&headers_str)
                .map_err(|err| err.to_string())
                .and_then(|s| toml::from_str(&s).map_err(|err| err.to_string()))
                .expect("REDDIT_HEADERS is not nor does it refer to a valid TOML document");
            headers_kv
                .into_iter()
                .map(|(k, v)| {
                    (
                        k.parse()
                            .expect(format!("{k} is not a valid header name").as_str()),
                        v.parse()
                            .expect(format!("{v} is not a valid header value").as_str()),
                    )
                })
                .collect()
        } else {
            [(
                header::USER_AGENT,
                concat!("server:rlamus:", env!("CARGO_PKG_VERSION"))
                    .parse()
                    .unwrap(),
            )]
            .into_iter()
            .collect()
        };
        Self {
            client: reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .unwrap(),
        }
    }
}

struct RedditUrl {
    subreddit: String,
    section: Option<RedditUrlSection>,
}

#[derive(Debug, PartialEq, Eq)]
enum RedditUrlSection {
    Comment {
        post_id: String,
        post_name: Option<String>,
    },
    Share {
        hash: String,
    },
}

impl SiteScraper for RedditSiteScraper {
    type Error = Error;

    fn name() -> &'static str {
        "reddit"
    }

    fn can_handle(&self, url: &Url) -> bool {
        tracing::trace!("can handle {url:?}?");
        match RedditUrl::try_from(url) {
            Ok(_) => true,
            Err(err) => {
                tracing::trace!("can't handle {url:?}: {err}");
                false
            }
        }
    }

    async fn scrape_markdown(&self, url: &Url) -> Result<ScrapeResult, Self::Error> {
        let subreddit = RedditUrl::try_from(url)?;
        fn get_content_as_codeblock(entry: &atom_syndication::Entry) -> String {
            if let Some(content) = entry.content()
                && let Some(content_value) = content.value()
            {
                let mut content_value = content_value.to_string();
                let mut content_type = content.content_type().unwrap_or("");
                match content_type {
                    "html" => {
                        if let Ok(markdown) = convert_html_to_md(&content_value) {
                            content_value = markdown;
                            content_type = "md";
                        }
                    }
                    _ => {}
                }
                format!(
                    include_str!("templates/code-block.md"),
                    content_type, content_value
                )
            } else {
                "".into()
            }
        }
        async fn fetch_feed(
            url: &Url,
            client: reqwest::Client,
        ) -> Result<atom_syndication::Feed, Error> {
            let mut url = url.clone();
            url.set_query(None);
            {
                let mut path_segments = url.path_segments_mut().unwrap();
                path_segments.pop_if_empty();
                path_segments.push(".rss");
            }
            tracing::trace!("fetching Reddit RSS feed from {url}");
            let mut response: reqwest::Response;
            loop {
                response = client.get(url.clone()).send().await?;
                if response.status().is_success() {
                    break;
                }
                if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    tracing::warn!("feed scrapping got rate limited, retrying in 10 seconds");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                } else {
                    response.error_for_status_ref()?;
                }
            }
            let feed = atom_syndication::Feed::read_from(response.bytes().await?.as_ref())?;
            Ok(feed)
        }
        fn comments(feed: atom_syndication::Feed) -> ScrapeResult {
            let op = feed.entries().first().unwrap();
            ScrapeResult {
                title: Some(feed.title.to_string()),
                content: format!(
                    include_str!("templates/reddit-comments.md"),
                    feed.title.as_str(),
                    op.authors
                        .first()
                        .map(|their| their.name())
                        .unwrap_or("unknown"),
                    feed.categories().first().unwrap().label().unwrap(),
                    feed.updated().to_rfc2822(),
                    get_content_as_codeblock(op),
                    feed.entries()
                        .iter()
                        .skip(1)
                        .enumerate()
                        .map(|(number, comment)| {
                            let content = get_content_as_codeblock(comment);
                            format!(
                                "{}. {} at {}\n\n{}",
                                number,
                                comment
                                    .authors()
                                    .first()
                                    .map(|their| their.name())
                                    .unwrap_or("unknown"),
                                comment.updated().to_rfc2822(),
                                content
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            }
        }
        fn subreddit_posts(feed: atom_syndication::Feed, subreddit: String) -> ScrapeResult {
            ScrapeResult {
                title: Some(format!("r/{}", subreddit)),
                content: format!(
                    include_str!("templates/reddit-subreddit.md"),
                    subreddit,
                    feed.subtitle().unwrap().to_string(),
                    feed.entries
                        .into_iter()
                        .map(|post| {
                            let content = get_content_as_codeblock(&post);
                            format!(
                                include_str!("templates/reddit-post.md"),
                                post.title.value,
                                post.authors
                                    .first()
                                    .map(|their| their.name())
                                    .unwrap_or("unknown"),
                                post.published().unwrap().to_rfc2822(),
                                content
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            }
        }

        match subreddit.section {
            Some(RedditUrlSection::Comment {
                post_id: _,
                post_name: _,
            }) => {
                let feed = fetch_feed(url, self.client.clone()).await?;
                Ok(comments(feed))
            }
            Some(RedditUrlSection::Share { hash: _ }) => {
                let response = self
                    .client
                    .head(url.clone())
                    .send()
                    .await?
                    .error_for_status()?;
                let url = response.url();
                tracing::debug!("share link redirected to {url}");
                let subreddit = RedditUrl::try_from(url)?;

                match subreddit.section {
                    Some(RedditUrlSection::Comment {
                        post_id: _,
                        post_name: _,
                    }) => {
                        let feed = fetch_feed(url, self.client.clone()).await?;
                        Ok(comments(feed))
                    }
                    Some(RedditUrlSection::Share { hash: _ }) => Err(Error::CircularRedirections),
                    None => {
                        let feed = fetch_feed(url, self.client.clone()).await?;
                        Ok(subreddit_posts(feed, subreddit.subreddit))
                    }
                }
            }
            None => {
                let feed = fetch_feed(url, self.client.clone()).await?;
                Ok(subreddit_posts(feed, subreddit.subreddit))
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(#[from] ParseRedditUrlError),
    #[error("http error: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("atom error: {0}")]
    AtomError(#[from] atom_syndication::Error),
    #[error("circular redirections")]
    CircularRedirections,
}

#[derive(Debug, Error)]
pub enum ParseRedditUrlError {
    #[error("unknown host: {0}")]
    UnknownHost(String),
    #[error("invalid path")]
    InvalidPath,
    #[error("missing {0}")]
    MissingPathSegment(&'static str),
    #[error("unsupported section: {0}")]
    UnsupportedSection(String),
}

impl TryFrom<&Url> for RedditUrl {
    type Error = ParseRedditUrlError;

    fn try_from(value: &Url) -> Result<Self, Self::Error> {
        let Some(host) = value.host_str() else {
            return Err(ParseRedditUrlError::UnknownHost("".into()));
        };
        if host != "reddit.com" && host != "www.reddit.com" {
            return Err(ParseRedditUrlError::UnknownHost(host.into()));
        }

        // https://www.reddit.com/r/neovim/
        let Some(mut segs) = value.path_segments() else {
            return Err(ParseRedditUrlError::InvalidPath);
        };
        if segs.next() != Some("r") {
            return Err(ParseRedditUrlError::InvalidPath);
        }
        let Some(subreddit) = segs.next() else {
            return Err(ParseRedditUrlError::MissingPathSegment("subreddit"));
        };
        if subreddit.is_empty() {
            return Err(ParseRedditUrlError::MissingPathSegment("subreddit"));
        }
        let Some(section) = segs.next() else {
            return Ok(Self {
                subreddit: subreddit.into(),
                section: None,
            });
        };
        match section {
            "comments" => {
                // 'https://www.reddit.com/r/neovim/comments/1ub8t5x/display_svg_icons_in_your_file_tree/'
                let Some(post_id) = segs.next() else {
                    return Err(ParseRedditUrlError::MissingPathSegment("post id"));
                };
                let Some(post_name) = segs.next() else {
                    return Ok(Self {
                        subreddit: subreddit.into(),
                        section: Some(RedditUrlSection::Comment {
                            post_id: post_id.into(),
                            post_name: None,
                        }),
                    });
                };
                return Ok(Self {
                    subreddit: subreddit.into(),
                    section: Some(RedditUrlSection::Comment {
                        post_id: post_id.into(),
                        post_name: Some(post_name.into()),
                    }),
                });
            }
            "s" => {
                let Some(hash) = segs.next() else {
                    return Err(ParseRedditUrlError::MissingPathSegment("hash"));
                };
                return Ok(Self {
                    subreddit: subreddit.into(),
                    section: Some(RedditUrlSection::Share { hash: hash.into() }),
                });
            }
            "" => {
                return Ok(Self {
                    subreddit: subreddit.into(),
                    section: None,
                });
            }
            _ => return Err(ParseRedditUrlError::UnsupportedSection(section.into())),
        }
    }
}

impl TryFrom<Url> for RedditUrl {
    type Error = ParseRedditUrlError;

    fn try_from(value: Url) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neovim_comments() {
        let url = Url::parse(
            "https://www.reddit.com/r/neovim/comments/1ub8t5x/display_svg_icons_in_your_file_tree/",
        )
        .unwrap();
        let reddit: RedditUrl = url.try_into().unwrap();
        assert_eq!(reddit.subreddit, "neovim");
        assert_eq!(
            reddit.section,
            Some(RedditUrlSection::Comment {
                post_id: "1ub8t5x".into(),
                post_name: Some("display_svg_icons_in_your_file_tree".into())
            })
        );
    }

    #[test]
    fn neovim() {
        let url = Url::parse("https://www.reddit.com/r/neovim/").unwrap();
        let reddit: RedditUrl = url.try_into().unwrap();
        assert_eq!(reddit.subreddit, "neovim");
        assert_eq!(reddit.section, None);
    }

    #[test]
    fn rust_comments() {
        let url = Url::parse("https://www.reddit.com/r/rust/s/QWqXrqVapJ").unwrap();
        let reddit: RedditUrl = url.try_into().unwrap();
        assert_eq!(reddit.subreddit, "rust");
        assert_eq!(
            reddit.section,
            Some(RedditUrlSection::Share {
                hash: "QWqXrqVapJ".into()
            })
        );
    }
}
