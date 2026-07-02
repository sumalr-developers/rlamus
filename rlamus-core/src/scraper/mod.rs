pub mod compatiblity;
pub mod reddit;
pub mod scraper;
pub mod youtube;

use html_to_markdown_rs::{ConversionOptions, LinkStyle};

pub use scraper::*;

pub struct ScrapeResult {
    pub content: String,
    pub title: Option<String>,
}

fn convert_html_to_md(content: &str) -> Result<String, Error> {
    let markdown = html_to_markdown_rs::convert(
        content,
        Some(
            ConversionOptions::builder()
                .link_style(LinkStyle::Reference)
                .extract_metadata(false)
                .skip_images(true)
                .build(),
        ),
    )?
    .content
    .unwrap();
    Ok(markdown)
}
