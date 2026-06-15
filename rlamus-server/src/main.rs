use std::{fmt::Display, ops::DerefMut, sync::Arc};

use axum::{
    Form, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::Parser;
use rlamus_core::{
    ollama::OllamaRunner,
    scraper::{
        Scraper,
        chromiumoxide::{BrowserConfig, handler::viewport::Viewport},
    },
    summarize::Summarize,
};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::{
    args::Args,
    task::{CachedRegistry, FsRegistry, Task, TaskRegistry, TaskState},
};

mod args;
mod task;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args = Args::parse();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(args.verbosity.tracing_level_filter().into())
                .from_env_lossy(),
        )
        .init();

    let app = app(
        CachedRegistry::new(FsRegistry::new(&args.data_dir)),
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
        OllamaRunner::default(),
    )
    .await
    .expect("Create app error");
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .expect("Failed to bind");
    tracing::debug!("Listening on {}", args.bind);
    axum::serve(listener, app).await.unwrap();
}

struct AppState<R> {
    registry: RwLock<R>,
    scraper: Scraper,
    summarizer: Summarize,
}

async fn app<R>(tasks: R, browser: BrowserConfig, ollama: OllamaRunner) -> anyhow::Result<Router>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Display,
{
    Ok(Router::new()
        .route("/task", post(task_create_handler))
        .route("/task/{id}", get(task_get_handler))
        .with_state(Arc::new(AppState {
            registry: RwLock::new(tasks),
            scraper: Scraper::launch_browser(browser, ollama.clone()).await?,
            summarizer: Summarize::new(ollama),
        })))
}

async fn task_create_handler<R>(
    State(app): State<Arc<AppState<R>>>,
    Form(input): Form<CreateTask>,
) -> Result<CreateTaskSuccess, R::Error>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Display,
{
    let mut task = Task::new(input.url.clone());
    let task_id = task.id.clone();
    app.registry.write().await.insert(task.clone()).await?;
    tokio::spawn(async move {
        task.state = TaskState::Scraping;
        update_task_in_registry(task.clone(), app.registry.write().await.deref_mut()).await;

        let doc = match app.scraper.get_markdown_uncropped(input.url).await {
            Ok(doc) => doc,
            Err(err) => {
                task.state = TaskState::Failed(format!("Page scraping failed: {err}").into());
                update_task_in_registry(task, app.registry.write().await.deref_mut()).await;
                return;
            }
        };

        task.state = TaskState::Summarizing;
        update_task_in_registry(task.clone(), app.registry.write().await.deref_mut()).await;
        let summary = app.summarizer.summarize(&doc).await;
        match summary {
            Ok(summary) => {
                task.state = TaskState::Done(summary.into());
            }
            Err(err) => {
                task.state = TaskState::Failed(format!("Summarization failed: {err}").into());
            }
        }
        update_task_in_registry(task, app.registry.write().await.deref_mut()).await;
    });
    Ok(CreateTaskSuccess {
        task_id: task_id.clone(),
    })
}

async fn update_task_in_registry<R>(task: Task, registry: &mut R)
where
    R: TaskRegistry,
    R::Error: Display,
{
    _ = registry
        .insert(task)
        .await
        .inspect_err(|err| tracing::error!("Failed to update task state: {err}"));
}

async fn task_get_handler<R>(
    State(app): State<Arc<AppState<R>>>,
    Path(id): Path<Uuid>,
) -> Result<GetTask, R::Error>
where
    R: TaskRegistry,
    R::Error: IntoResponse,
{
    Ok(app.registry.read().await.get(&id).await?.into())
}

#[derive(Debug, Deserialize)]
struct CreateTask {
    url: String,
}

struct CreateTaskSuccess {
    task_id: Uuid,
}

enum GetTask {
    NotFound,
    Found(Task),
}

impl IntoResponse for CreateTaskSuccess {
    fn into_response(self) -> Response {
        Response::builder()
            .status(StatusCode::CREATED)
            .body(self.task_id.to_string().into())
            .unwrap()
    }
}

impl IntoResponse for GetTask {
    fn into_response(self) -> Response {
        match self {
            GetTask::NotFound => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("Task not found".into())
                .unwrap(),
            GetTask::Found(task) => Json(task).into_response(),
        }
    }
}

impl From<Option<Task>> for GetTask {
    fn from(value: Option<Task>) -> Self {
        value.map(Self::Found).unwrap_or(Self::NotFound)
    }
}
