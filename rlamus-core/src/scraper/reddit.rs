use reqwest::header::{self, HeaderMap};
use thiserror::Error;
use url::Url;

use crate::scraper::{compatiblity::SiteScraper, convert_html_to_md};

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
        let headers: HeaderMap = [(
            header::USER_AGENT,
            concat!("server:rlamus:", env!("CARGO_PKG_VERSION"))
                .parse()
                .unwrap(),
        )]
        .into_iter()
        .collect();
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

enum RedditUrlSection {
    Comment {
        post_id: String,
        post_name: Option<String>,
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

    async fn scrape_markdown(&self, url: &Url) -> Result<String, Self::Error> {
        let subreddit = RedditUrl::try_from(url)?;
        let mut url = url.clone();
        url.path_segments_mut().unwrap().push(".rss");
        let response = self.client.get(url).send().await?;
        response.error_for_status_ref()?;
        let feed = atom_syndication::Feed::read_from(response.bytes().await?.as_ref())?;
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
        match subreddit.section {
            Some(RedditUrlSection::Comment {
                post_id: _,
                post_name: _,
            }) => {
                let op = feed.entries().first().unwrap();
                Ok(format!(
                    include_str!("templates/reddit-comments.md"),
                    feed.title.as_str(),
                    op.authors.first().unwrap().name,
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
                                comment.authors().first().unwrap().name,
                                comment.updated().to_rfc2822(),
                                content
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
            }
            None => {
                // the url is a subreddit
                Ok(format!(
                    include_str!("templates/reddit-subreddit.md"),
                    subreddit.subreddit,
                    feed.subtitle().unwrap().to_string(),
                    feed.entries
                        .into_iter()
                        .map(|post| {
                            let content = get_content_as_codeblock(&post);
                            format!(
                                include_str!("templates/reddit-post.md"),
                                post.title.value,
                                post.authors.first().unwrap().name,
                                post.published().unwrap().to_rfc2822(),
                                content
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
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

        // 'https://www.reddit.com/r/neovim/.rss
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
                // 'https://www.reddit.com/r/neovim/comments/1ub8t5x/display_svg_icons_in_your_file_tree/.rss'
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
