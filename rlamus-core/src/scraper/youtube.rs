use std::path::PathBuf;

use rustypipe::{client::RustyPipe, model::richtext::ToMarkdown};
use thiserror::Error;
use url::Url;

use crate::scraper::{ScrapeResult, compatiblity::SiteScraper};

#[derive(Default)]
pub struct YouTubeSiteScraper {
    rp: RustyPipe,
    http: reqwest::Client,
}

impl YouTubeSiteScraper {
    pub fn new(storage_dir: impl Into<PathBuf>) -> Self {
        Self {
            rp: RustyPipe::builder()
                .storage_dir(storage_dir)
                .build()
                .unwrap(),
            http: reqwest::Client::new(),
        }
    }
}

impl SiteScraper for YouTubeSiteScraper {
    type Error = Error;

    fn name() -> &'static str {
        "youtube"
    }

    fn can_handle(&self, url: &url::Url) -> bool {
        match YouTubeUrl::try_from(url) {
            Ok(YouTubeUrl::Video { id: _ } | YouTubeUrl::Short { id: _ }) => true,
            _ => false,
        }
    }

    async fn scrape_markdown(&self, url: &url::Url) -> Result<super::ScrapeResult, Self::Error> {
        let id = match YouTubeUrl::try_from(url)? {
            YouTubeUrl::Video { id } => id,
            YouTubeUrl::Short { id } => id,
            _ => return Err(Error::UnsupportedUrl),
        };

        let player = self.rp.query().player(&id).await?;
        let subtitle = player
            .audio_streams
            .first()
            .and_then(|it| it.track.as_ref().map(|it| it.lang_name.as_ref()))
            .and_then(|primary_language_name| {
                player
                    .subtitles
                    .iter()
                    .find(|it| it.lang_name == primary_language_name)
            })
            .or_else(|| player.subtitles.first());

        let subtitle_content = if let Some(subtitle) = subtitle {
            let respone = self
                .http
                .get({
                    let mut url = Url::parse(&subtitle.url).unwrap();
                    url.query_pairs_mut().append_pair("fmt", "vtt");
                    url
                })
                .send()
                .await?
                .error_for_status()?;
            Some(
                respone
                    .text()
                    .await?
                    .split("\n")
                    .fold("".to_string(), |acc, x| {
                        if acc.len() + x.len() < 50_000 {
                            if acc.ends_with("\n") {
                                format!("{acc}{x}")
                            } else {
                                format!("{acc}\n{x}")
                            }
                        } else {
                            acc
                        }
                    }),
            )
        } else {
            None
        };
        let video_details = self.rp.query().video_details(&id).await?;
        let duration = {
            let mut seconds = player.details.duration;
            let mut minutes = 0u32;
            let mut hours = 0u32;
            while seconds >= 60 {
                minutes += 1;
                seconds -= 60;
            }
            while minutes >= 60 {
                hours += 1;
                minutes -= 60;
            }
            format!("{hours}:{minutes:02}:{seconds:02}")
        };
        Ok(ScrapeResult {
            title: player
                .details
                .name
                .map(|title| format!("{} - YouTube", title)),
            content: format!(
                include_str!("templates/youtube-video.md"),
                id,
                video_details.view_count,
                video_details
                    .like_count
                    .map(|it| it.to_string())
                    .unwrap_or("unknown".into()),
                video_details.channel.name,
                duration,
                video_details.name,
                video_details.description.to_markdown(),
                subtitle_content.unwrap_or_default(),
            ),
        })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("parse URL: {0}")]
    ParseUrl(#[from] ParseYouTubeUrlError),
    #[error("unsupported URL")]
    UnsupportedUrl,
    #[error("player: {0}")]
    RustyPipe(#[from] rustypipe::error::Error),
    #[error("fetching subtitle: {0}")]
    FetchingSubtitle(#[from] reqwest::Error),
}

#[derive(Debug, PartialEq, Eq)]
enum YouTubeUrl {
    Video { id: String },
    Short { id: String },
    Root,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseYouTubeUrlError {
    #[error("unknown host: {0:?}")]
    UnknownHost(String),
    #[error("missing parameter {0}")]
    MissingParameter(&'static str),
    #[error("missing path segment")]
    MissingPathSegment,
}

impl TryFrom<&url::Url> for YouTubeUrl {
    type Error = ParseYouTubeUrlError;

    fn try_from(value: &url::Url) -> Result<Self, Self::Error> {
        let Some(host) = value.host_str() else {
            return Err(ParseYouTubeUrlError::UnknownHost("".into()));
        };
        match host {
            "www.youtube.com" | "youtube.com" => {
                let Some(mut paths) = value.path_segments() else {
                    return Ok(Self::Root);
                };
                match paths.next() {
                    Some("watch") => {
                        let Some((_, vid)) = value.query_pairs().find(|(name, _)| name == "v")
                        else {
                            return Err(ParseYouTubeUrlError::MissingParameter("v"));
                        };
                        return Ok(Self::Video { id: vid.into() });
                    }
                    Some("shorts") => {
                        let Some(vid) = paths.next() else {
                            return Err(ParseYouTubeUrlError::MissingPathSegment);
                        };
                        return Ok(Self::Short { id: vid.into() });
                    }
                    _ => return Ok(Self::Root),
                }
            }
            "youtu.be" => {
                let Some(mut paths) = value.path_segments() else {
                    return Ok(Self::Root);
                };
                let Some(vid) = paths.next() else {
                    return Ok(Self::Root);
                };
                if vid.is_empty() {
                    return Ok(Self::Root);
                }

                return Ok(Self::Video { id: vid.into() });
            }
            _ => return Err(ParseYouTubeUrlError::UnknownHost(host.into())),
        }
    }
}

impl TryFrom<url::Url> for YouTubeUrl {
    type Error = ParseYouTubeUrlError;

    fn try_from(value: url::Url) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn parse(s: &str) -> Result<YouTubeUrl, ParseYouTubeUrlError> {
        let url = Url::parse(s).expect("valid url in test fixture");
        YouTubeUrl::try_from(&url)
    }

    // --- www.youtube.com / youtube.com ---

    #[test]
    fn watch_url_with_www_returns_video() {
        assert_eq!(
            parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            YouTubeUrl::Video {
                id: "dQw4w9WgXcQ".to_string()
            }
        );
    }

    #[test]
    fn watch_url_without_www_returns_video() {
        assert_eq!(
            parse("https://youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            YouTubeUrl::Video {
                id: "dQw4w9WgXcQ".to_string()
            }
        );
    }

    #[test]
    fn watch_url_picks_v_param_among_others() {
        assert_eq!(
            parse("https://www.youtube.com/watch?t=42&v=abc123&list=xyz").unwrap(),
            YouTubeUrl::Video {
                id: "abc123".to_string()
            }
        );
    }

    #[test]
    fn watch_url_missing_v_param_errors() {
        assert_eq!(
            parse("https://www.youtube.com/watch").unwrap_err(),
            ParseYouTubeUrlError::MissingParameter("v")
        );
    }

    #[test]
    fn watch_url_with_unrelated_query_errors() {
        assert_eq!(
            parse("https://www.youtube.com/watch?t=42").unwrap_err(),
            ParseYouTubeUrlError::MissingParameter("v")
        );
    }

    #[test]
    fn watch_url_with_empty_v_param_returns_empty_id() {
        assert_eq!(
            parse("https://www.youtube.com/watch?v=").unwrap(),
            YouTubeUrl::Video { id: String::new() }
        );
    }

    #[test]
    fn bare_domain_returns_root() {
        assert_eq!(parse("https://www.youtube.com").unwrap(), YouTubeUrl::Root);
    }

    #[test]
    fn trailing_slash_returns_root() {
        assert_eq!(parse("https://www.youtube.com/").unwrap(), YouTubeUrl::Root);
    }

    #[test]
    fn unrelated_path_returns_root() {
        assert_eq!(
            parse("https://www.youtube.com/feed/subscriptions").unwrap(),
            YouTubeUrl::Root
        );
    }

    // --- youtu.be ---

    #[test]
    fn youtu_be_short_link_returns_video() {
        assert_eq!(
            parse("https://youtu.be/dQw4w9WgXcQ").unwrap(),
            YouTubeUrl::Video {
                id: "dQw4w9WgXcQ".to_string()
            }
        );
    }

    #[test]
    fn youtu_be_ignores_extra_path_segments() {
        assert_eq!(
            parse("https://youtu.be/dQw4w9WgXcQ/extra/stuff").unwrap(),
            YouTubeUrl::Video {
                id: "dQw4w9WgXcQ".to_string()
            }
        );
    }

    #[test]
    fn youtu_be_ignores_query_string() {
        assert_eq!(
            parse("https://youtu.be/dQw4w9WgXcQ?t=30").unwrap(),
            YouTubeUrl::Video {
                id: "dQw4w9WgXcQ".to_string()
            }
        );
    }

    #[test]
    fn youtu_be_bare_domain_yields_root() {
        assert_eq!(parse("https://youtu.be/").unwrap(), YouTubeUrl::Root,);
    }

    // --- errors ---

    #[test]
    fn unknown_host_errors() {
        assert_eq!(
            parse("https://example.com/watch?v=abc123").unwrap_err(),
            ParseYouTubeUrlError::UnknownHost("example.com".to_string())
        );
    }

    #[test]
    fn no_host_errors_with_empty_string() {
        let url = Url::parse("mailto:someone@example.com").unwrap();
        assert_eq!(
            YouTubeUrl::try_from(&url).unwrap_err(),
            ParseYouTubeUrlError::UnknownHost(String::new())
        );
    }

    // --- owned TryFrom<Url> ---

    #[test]
    fn owned_url_try_from_matches_reference_impl() {
        let url = Url::parse("https://youtu.be/dQw4w9WgXcQ").unwrap();
        assert_eq!(
            YouTubeUrl::try_from(url).unwrap(),
            YouTubeUrl::Video {
                id: "dQw4w9WgXcQ".to_string()
            }
        );
    }

    // --- error message formatting ---

    #[test]
    fn missing_parameter_error_message() {
        let err = ParseYouTubeUrlError::MissingParameter("v");
        assert_eq!(err.to_string(), "missing parameter v");
    }

    #[test]
    fn unknown_host_error_message() {
        let err = ParseYouTubeUrlError::UnknownHost("example.com".to_string());
        assert_eq!(err.to_string(), r#"unknown host: "example.com""#);
    }
}
