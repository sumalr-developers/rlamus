use std::env;

use tracing::{Instrument, Level, event, info_span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use rlamus_core::{
    ollama::OllamaRunner,
    scraper::{
        Scraper,
        chromiumoxide::{BrowserConfig, handler::viewport::Viewport},
    },
    summarize::Summarize,
};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let runner = OllamaRunner::default();
    let scraper = Scraper::launch_browser(
        BrowserConfig::builder()
            .chrome_executable(
                std::env::var("CHROMIUM_BIN").expect("Missing CHROMIUM_BIN environment variable"),
            )
            .viewport(Some(Viewport {
                width: 1280,
                height: 1280,
                device_scale_factor: None,
                emulating_mobile: false,
                is_landscape: true,
                has_touch: false,
            }))
            .build()
            .unwrap(),
        runner.clone(),
    )
    .instrument(info_span!("scraper"))
    .await
    .expect("Failed to launch scraper");
    let Some(url) = env::args().skip(1).next() else {
        eprintln!("Missing argument 1 for URL");
        return;
    };
    let doc = scraper
        .get_markdown_uncropped(url)
        .await
        .expect("Read URL failed");
    event!(Level::TRACE, "document: {doc}");
    let summarizer = Summarize::new(runner);
    let summary = summarizer
        .summarize(&doc.content)
        .await
        .expect("Summarize failed");
    if let Some(title) = doc.title {
        println!("# {}", title);
    }
    println!("{}", summary);
}
