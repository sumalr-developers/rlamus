use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    hash::Hash,
    time::Duration,
};

use ab_glyph::FontRef;
use base64::{Engine, prelude::BASE64_STANDARD};
use chromiumoxide::{
    Browser, BrowserConfig, Page,
    cdp::browser_protocol::{
        page::{CaptureScreenshotFormat, CaptureScreenshotParams, NavigateParams, Viewport},
        target::CreateTargetParams,
    },
    error::CdpError,
};
use futures::StreamExt;
use imageproc::{
    drawing,
    image::{self, ImageFormat, RgbImage, codecs::png::PngEncoder},
    rect::Rect,
};
use ollama_rs::{
    error::OllamaError,
    generation::{
        chat::{ChatMessage, request::ChatMessageRequest},
        images::Image,
        parameters::ThinkType,
    },
};
use regex::bytes::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{Instrument, Level, event, event_enabled, info_span};

use crate::{
    ollama::OllamaRunner,
    scraper::{
        ScrapeResult,
        compatiblity::{self, CompatibilityLayer},
        convert_html_to_md,
    },
};

pub use chromiumoxide;

pub struct Scraper {
    browser: Browser,
    handle: tokio::task::JoinHandle<()>,
    runner: OllamaRunner,
    compatibiltiy_layer: CompatibilityLayer,
    min_len: usize,
    max_len: usize,
    max_iterations: usize,
}

impl Scraper {
    pub async fn launch_browser(
        config: BrowserConfig,
        runner: impl Into<OllamaRunner>,
    ) -> Result<Self, CdpError> {
        let (browser, mut handler) = Browser::launch(config).await?;
        Ok(Self {
            browser,
            handle: tokio::spawn(async move { while handler.next().await.is_some() {} }),
            runner: runner.into(),
            compatibiltiy_layer: CompatibilityLayer::default(),
            min_len: 50,
            max_len: 5_000,
            max_iterations: 5,
        })
    }

    pub fn max_len(mut self, limit: usize) -> Self {
        self.max_len = limit;
        self
    }

    pub fn min_len(mut self, limit: usize) -> Self {
        self.min_len = limit;
        self
    }

    pub fn max_iterations(mut self, limit: usize) -> Self {
        self.max_iterations = limit;
        self
    }

    pub fn compatibility_layer(mut self, value: CompatibilityLayer) -> Self {
        self.compatibiltiy_layer = value;
        self
    }

    async fn get_markdown_maybe_imcomplete(&self, page: &Page) -> Result<String, Error> {
        // Prioritize *the* main tag
        if let Ok(main_tags) = page.find_elements("main").await
            && main_tags.len() == 1
        {
            let tag = main_tags.first().unwrap();
            let content = tag.inner_html().await?;
            if let Some(content) = content {
                tracing::trace!("using <main> tag");
                let markdown = convert_html_to_md(&content)?;
                return Ok(markdown);
            }
        }

        // If the page is small enough, just YOLO it
        let raw_markdown = convert_html_to_md(page.content().await?.as_str())?;
        return Ok(raw_markdown);
    }

    pub async fn get_markdown_uncropped(
        &self,
        url: impl Into<NavigateParams>,
    ) -> Result<ScrapeResult, Error> {
        let nav_destination = url.into();
        let original_url = nav_destination.url.clone();
        match self
            .compatibiltiy_layer
            .scrape_markdown(&original_url)
            .instrument(tracing::info_span!("compatible"))
            .await
        {
            Ok(scrape) => {
                tracing::info!("skipping web scraping in favor of compatibility layer");
                tracing::trace!("compatible result: {}", scrape.content);
                return Ok(scrape);
            }
            Err(compatiblity::Error::CannotHandle) => {
                // ignored
            }
            Err(err) => {
                tracing::warn!("compatibility layer failed: {err}");
            }
        }
        let page = self
            .browser
            .new_page(
                CreateTargetParams::builder()
                    .url("about:blank") // Will enable stealth mode later
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
        page.enable_stealth_mode().await?;
        page.goto(nav_destination)
            .await?
            .wait_for_navigation()
            .await?;
        let page_title = async || page.get_title().await.ok().flatten();

        if let Ok(Some(url)) = page.url().await
            && url != original_url
        {
            match self
                .compatibiltiy_layer
                .scrape_markdown(&url)
                .instrument(tracing::info_span!("compatible"))
                .await
            {
                Ok(markdown) => {
                    return Ok(markdown);
                }
                Err(compatiblity::Error::CannotHandle) => {
                    // ignored
                }
                Err(err) => {
                    tracing::warn!("compatibility layer failed: {err}");
                }
            }
        }

        // wait for at most 10 seconds for the page to load (javascript)
        for _ in 0..5 {
            match self.get_markdown_maybe_imcomplete(&page).await {
                Ok(markdown) => {
                    if (self.min_len..=self.max_len).contains(&markdown.len()) {
                        return Ok(ScrapeResult {
                            content: markdown,
                            title: page_title().await,
                        });
                    }
                    tracing::trace!("wait 2 seconds for page to load");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(err) => {
                    tracing::warn!("premature optimization failed: {err}")
                }
            }
        }

        let font = FontRef::try_from_slice(include_bytes!("MapleMono-Bold.otf")).unwrap();
        for iter_num in 0..self.max_iterations {
            let css = page.layout_metrics().await?;
            let viewport_default = {
                let vp = css.css_visual_viewport;
                Rect::at(vp.offset_x.round() as i32, vp.offset_y.round() as i32).of_size(
                    vp.client_width.round() as u32,
                    vp.client_height.round() as u32,
                )
            };
            let sections: Vec<BoundingRect> = page
                .evaluate_expression(js_func_call(
                    include_str!("split-element.js"),
                    "document.body",
                ))
                .await?
                .into_value()
                .unwrap();
            const PADDING: u32 = 12;
            let sections: Vec<Section> = sections
                .iter()
                .filter(|it| {
                    it.width as u32 >= PADDING
                        && it.height as u32 >= PADDING
                        && it.left + it.width > 0f32
                        && it.top + it.height > 0f32
                        && it.left < viewport_default.right() as f32
                        && it.top < viewport_default.bottom() as f32
                })
                .enumerate()
                .map(|(idx, it)| Section {
                    bounds: it.clone().into(),
                    id: idx as u32 + 1,
                    js_id: it.id,
                })
                .collect();
            if sections.is_empty() {
                event!(Level::INFO, "iterations ended with no sections");
                return Ok(ScrapeResult {
                    content: convert_html_to_md(page.content().await?.as_str())?,
                    title: page_title().await,
                });
            }
            let mut unannotated: BinaryHeap<Reverse<&Section>> =
                sections.iter().map(Reverse).collect();
            let mut screenshots = Vec::new();
            while let Some(Reverse(first)) = unannotated.peek() {
                let viewport_rect = Rect::at(
                    unannotated
                        .iter()
                        .map(|Reverse(it)| it.bounds.left())
                        .min()
                        .unwrap(),
                    if first.bounds.top() > (viewport_default.height() / 4) as i32 {
                        (first.bounds.bottom() - viewport_default.height() as i32).max(0)
                    } else {
                        0
                    },
                )
                .of_size(viewport_default.width(), viewport_default.height());
                let mut screenshot = take_screenshot(&page, viewport_rect).await?;
                let annotated = info_span!("annotate_screenshot", viewport = ?viewport_rect)
                    .in_scope(|| {
                        annotate_screenshot(
                            &mut screenshot,
                            &sections,
                            viewport_rect,
                            PADDING,
                            font.clone(),
                        )
                    });
                screenshots.push(screenshot);
                unannotated.retain(|Reverse(it)| !annotated.contains(&it.id));
            }
            if event_enabled!(Level::TRACE) {
                for (idx, screenshot) in screenshots.iter().enumerate() {
                    let filename = format!("scraper iter {} split.{}.png", iter_num, idx);
                    tracing::trace!("saving screenshot to {filename:?}");
                    screenshot.save(filename).unwrap();
                }
            }
            let mut history = vec![];
            let res = self
            .runner
            .ollama
            .send_chat_messages_with_history(&mut history, {
                let msg = ChatMessage::user(format!(
                        "{}You're tasked with identifying the main section where key info lives, usually richest in content. There are {} sections, each marked by a red rectangle and a center aligned label. Respond with a number. No explanations.", 
                        page_title().await.as_ref().map(|title| format!("Page's titled {title:?}. ")).unwrap_or("".into()),
                        sections.len())
                    ).with_images(screenshots.into_iter().map(|it| {
                    let mut buf = vec![];
                    it.write_with_encoder(PngEncoder::new(&mut buf)).expect("Unable to encode screenshot as PNG");
                    Image::from_base64(
                        BASE64_STANDARD.encode(&buf),
                    )
                }).collect());
                ChatMessageRequest::new(self.runner.model.clone(), vec![msg])
                    .think(ThinkType::High)
            })
            .await?;
            event!(Level::TRACE, "main section resposne: {res:#?}");
            let Some(main_section) = parse_model_section_res(&res.message.content)
                .and_then(|it| if it == 0 { None } else { Some(it) })
            else {
                return Err(Error::InvalidResponse(res.message.content));
            };
            let main_section = sections[main_section as usize - 1];
            event!(Level::TRACE, "main section is {main_section:?}");
            page.evaluate_expression(js_func_call(
                include_str!("delete-split-except.js"),
                format!("{}", main_section.js_id),
            ))
            .await?;
            if event_enabled!(Level::TRACE) {
                let filename = format!("scraper iter {iter_num} cleanup.png");
                tracing::trace!("saving screenshot to {filename:?}");
                page.save_screenshot(
                    CaptureScreenshotParams::builder()
                        .format(CaptureScreenshotFormat::Png)
                        .build(),
                    filename,
                )
                .await
                .unwrap();
            }
            let markdown = convert_html_to_md(page.content().await?.as_str())?;
            if markdown.len() <= self.max_len || iter_num == self.max_iterations - 1 {
                if markdown.len() > self.max_len {
                    event!(Level::INFO, "iterations ended with limit")
                }
                event!(Level::TRACE, "final iteration is {iter_num}");
                return Ok(ScrapeResult {
                    content: markdown,
                    title: page_title().await,
                });
            }

            let res = self.runner
            .ollama
            .send_chat_messages_with_history(&mut history, {
                let msg = ChatMessage::user(
                    "Is this suitable to be partitioned, so that one of the parts retains all the important information?\n\nRespond in \"Yes\" or \"No\""
                        .into()
                );
                ChatMessageRequest::new(self.runner.model.clone(), vec![msg]).think(ThinkType::High)
            }).await?;
            event!(Level::TRACE, "partitionable resposne: {res:#?}");
            let Some(partitionable) = parse_model_yes_or_no_res(&res.message.content) else {
                return Err(Error::InvalidResponse(res.message.content));
            };
            if !partitionable {
                event!(Level::INFO, "iterations ended with no more partitions");
                event!(Level::TRACE, "final iteration is {iter_num}");
                return Ok(ScrapeResult {
                    content: markdown,
                    title: page_title().await,
                });
            }
        }
        page.close().await?;
        panic!("Too many iterations");
    }

    pub async fn get_markdown(
        &self,
        url: impl Into<NavigateParams>,
    ) -> Result<ScrapeResult, Error> {
        let scrape = self.get_markdown_uncropped(url).await?;
        Ok(ScrapeResult {
            content: scrape.content.chars().take(self.max_len).collect(),
            title: scrape.title,
        })
    }
}

async fn take_screenshot(page: &Page, viewport: Rect) -> Result<RgbImage, CdpError> {
    let screenshot = page
        .screenshot(
            CaptureScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .capture_beyond_viewport(true)
                .clip(
                    Viewport::builder()
                        .x(viewport.left())
                        .y(viewport.top())
                        .scale(1)
                        .width(viewport.width())
                        .height(viewport.height())
                        .build()
                        .unwrap(),
                )
                .build(),
        )
        .await?;
    let screenshot = image::load_from_memory_with_format(&screenshot, ImageFormat::Png)
        .expect("Unable to load screenshot")
        .to_rgb8();
    Ok(screenshot)
}

/**
 * Annotate screenshot with section numbers (id).
 * Return a set of annotated ids.
 */
fn annotate_screenshot<'a>(
    screenshot: &mut RgbImage,
    sections: impl IntoIterator<Item = &'a Section>,
    viewport: Rect,
    padding: u32,
    font: FontRef,
) -> HashSet<u32> {
    const RED: image::Rgb<u8> = image::Rgb([255, 0, 0]);
    const GREEN: image::Rgb<u8> = image::Rgb([0, 255, 0]);
    const BLUE: image::Rgb<u8> = image::Rgb([0, 0, 255]);
    const PURPLE: image::Rgb<u8> = image::Rgb([255, 0, 255]);
    const YELLOW: image::Rgb<u8> = image::Rgb([255, 255, 0]);
    const CYAN: image::Rgb<u8> = image::Rgb([0, 255, 255]);
    const FRAME_COLORS: [image::Rgb<u8>; 6] = [RED, GREEN, BLUE, PURPLE, YELLOW, CYAN];
    const WHITE: image::Rgb<u8> = image::Rgb([255, 255, 255]);
    let mut annotated = HashSet::new();
    for (section_id, section) in sections.into_iter().enumerate() {
        let direct: Rect = section.bounds.clone();
        let bounds: Rect = {
            let Some(result) = viewport.intersect(direct) else {
                continue;
            };
            event!(Level::DEBUG, "intersection: {result:?}");
            Rect::at(
                result.left() - viewport.left(),
                result.top() - viewport.top(),
            )
            .of_size(result.width(), result.height())
        };
        if bounds.width() >= padding * 2 && bounds.height() >= padding * 2
            || direct.width() < padding * 2
            || direct.height() < padding * 2
        {
            // Considered annotated only if it is large enough, or the original bounds is too small
            annotated.insert(section.id);
        }

        drawing::draw_hollow_rect_mut(screenshot, bounds, FRAME_COLORS[section_id % FRAME_COLORS.len()]);
        let section_text = format!("Section {}", section.id);
        let mut scale = 24f32;
        let (mut width, mut height) = drawing::text_size(scale, &font, &section_text);
        while scale > 12f32 {
            if width + padding > bounds.width() || height + padding > bounds.height() {
                scale -= 1f32;
            } else {
                break;
            }
            (width, height) = drawing::text_size(scale, &font, &section_text);
        }
        let draw_area = Rect::at(
            bounds.left()
                + ((bounds.width() as f32 - width as f32) / 2f32 - padding as f32).round() as i32,
            (bounds.top()
                + ((bounds.height() as f32 - height as f32 - padding as f32) / 2f32
                    - padding as f32)
                    .round() as i32)
                .min(viewport.height() as i32 - padding as i32 * 3 - height as i32),
        )
        .of_size(width + padding * 2, height + padding * 2);
        drawing::draw_filled_rect_mut(screenshot, draw_area, FRAME_COLORS[section_id % FRAME_COLORS.len()]);
        drawing::draw_text_mut(
            screenshot,
            WHITE,
            draw_area.left() + padding as i32,
            draw_area.top() + padding as i32,
            scale,
            &font,
            &section_text,
        );
    }
    annotated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Section {
    bounds: Rect,
    id: u32,
    js_id: u32,
}

fn parse_model_section_res(content: &str) -> Option<u32> {
    let re = Regex::new(r#"([sS]ection)? ?(\d+)"#).unwrap();
    let Some(cap) = re.captures_iter(content.as_bytes()).last() else {
        return None;
    };
    cap.get(2)
        .and_then(|it| str::from_utf8(it.as_bytes()).ok())
        .and_then(|it| it.parse::<u32>().ok())
}

fn parse_model_yes_or_no_res(content: &str) -> Option<bool> {
    if content.contains("Yes") {
        return Some(true);
    } else if content.contains("No") {
        return Some(false);
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoundingRect {
    width: f32,
    height: f32,
    left: f32,
    top: f32,
    id: u32,
}

impl Into<Rect> for BoundingRect {
    fn into(self) -> Rect {
        Rect::at(self.left.round() as i32, self.top.round() as i32)
            .of_size(self.width.round() as u32, self.height.round() as u32)
    }
}

fn js_func_call(source: impl ToString, params: impl AsRef<str>) -> String {
    let mut source = source.to_string().trim_end().to_string();
    while source.ends_with(';') {
        source = source[..source.len() - 1].to_string();
    }
    format!("({source})({})", params.as_ref())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("CDP: {0}")]
    Cdp(#[from] CdpError),
    #[error("Ollama: {0}")]
    Ollama(#[from] OllamaError),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("HTML to Markdown: {0}")]
    Conversion(#[from] html_to_markdown_rs::ConversionError),
}

impl Hash for Section {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Ord for Section {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let x_1 = self.bounds.top() as f32 + self.bounds.width() as f32 / 2f32;
        let y_1 = self.bounds.left() as f32 + self.bounds.height() as f32 / 2f32;
        let x_2 = other.bounds.top() as f32 + other.bounds.width() as f32 / 2f32;
        let y_2 = other.bounds.left() as f32 + other.bounds.height() as f32 / 2f32;

        (x_1 + y_1).total_cmp(&(x_2 + y_2))
    }
}

impl PartialOrd for Section {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
