use std::{
    borrow::Cow,
    convert::Infallible,
    fmt::{Debug, Display},
    future,
    ops::DerefMut,
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
use axum::{
    Form, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response, Sse, sse},
    routing::{delete, get, post},
};
use clap::{CommandFactory, Parser};
use futures::{Stream, StreamExt};
use rlamus_core::{
    environ,
    ollama::OllamaRunner,
    scraper::{
        Scraper,
        chromiumoxide::{BrowserConfig, handler::viewport::Viewport},
    },
    summarize::Summarize,
};
use serde::Deserialize;
use smol_str::ToSmolStr;
use tokio::sync::Mutex;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::{
    args::Args,
    task::{CachedRegistry, FsRegistry, Task, TaskRegistry, TaskState, expire::Expire},
};

mod args;
mod push;
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

    let (app, state) = init(
        CachedRegistry::new(FsRegistry::new(&args.data_dir)),
        BrowserConfig::builder()
            .chrome_executable({
                let Some(path) = args
                    .chromium_binary
                    .and_then(|path| path.to_str().map(|s| s.to_owned()))
                    .or_else(|| std::env::var("CHROMIUM_BIN").ok())
                else {
                    let mut args = Args::command();
                    args.error(clap::error::ErrorKind::MissingRequiredArgument,
                        "Missing --chromium-bin CLI argument and CHROMIUM_BIN environment variable. Speicify one")
                        .exit();
                };
                path
            })
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
        args.apn
    )
    .await
    .expect("Init app error");
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .expect("Failed to bind");
    tracing::debug!("listening on {}", args.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_handler(state))
        .await
        .unwrap();
}

struct AppState<R: TaskRegistry> {
    registry: Arc<R>,
    expire: Mutex<Expire<R>>,
    scraper: Scraper,
    summarizer: Summarize,
    apn_client: Option<Arc<apns_h2::Client>>,
}

async fn init<R>(
    tasks: R,
    browser: BrowserConfig,
    ollama: OllamaRunner,
    apn: args::Apn,
) -> anyhow::Result<(Router, Arc<AppState<R>>)>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Send + Sync + std::error::Error,
{
    use apns_h2::ClientConfig;
    let apn_config = ClientConfig::new(if apn.apn_sandbox {
        apns_h2::Endpoint::Sandbox
    } else {
        apns_h2::Endpoint::Production
    });
    let apn_client = match (apn.certificate, apn.token) {
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!(),
        (None, Some(token)) => {
            use apns_h2::Client;
            use std::fs::File;
            Some(Client::token(
                File::open(token.apn_p8)?,
                token.apn_p8_key_id,
                token.apn_p8_team_id,
                apn_config,
            )?)
        }
        (Some(cert), None) => {
            use apns_h2::Client;
            use std::fs::File;
            let env_password = std::env::var_os("APN_P12_PASSWORD");
            let password = env_password
                .as_ref()
                .map(|s| environ::from_os_str(s).map_err(|err| anyhow!(err)))
                .unwrap_or_else(|| {
                    cert.apn_p12_password
                        .map(Cow::Owned)
                        .ok_or(anyhow!("Missing --apn-p12-password and APN_P12_PASSWORD environment variable. Specific one"))
                })?;
            Some(Client::certificate(
                &mut File::open(cert.apn_p12)?,
                &password,
                apn_config,
            )?)
        }
    };
    let registry = Arc::new(tasks);
    let state = Arc::new(AppState {
        registry: Arc::clone(&registry),
        // tasks expire in 4 hours
        expire: Mutex::new(Expire::new(registry, Duration::from_secs(60 * 60 * 4))),
        scraper: Scraper::launch_browser(browser, ollama.clone()).await?,
        summarizer: Summarize::new(ollama),
        apn_client: apn_client.map(Arc::new),
    });
    if let Err(err) = state.expire.lock().await.start().await {
        tracing::error!("failed to start expirer: {err}");
    }
    Ok((
        Router::new()
            .route("/", get(root_handler))
            .route("/task", post(task_create_handler))
            .route("/task/{id}", get(task_get_handler))
            .route("/task/{id}", delete(task_delete_handler))
            .route("/task/{id}/sse", get(task_sse_get_handler))
            .with_state(Arc::clone(&state)),
        state,
    ))
}

async fn root_handler() -> &'static str {
    "rlamus-server api:1"
}

async fn task_create_handler<R>(
    State(app): State<Arc<AppState<R>>>,
    Form(input): Form<CreateTask>,
) -> Result<CreateTaskSuccess, R::Error>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Display + Send,
{
    let mut task = Task::new(input.url.clone());
    let task_id = task.id.clone();
    app.registry.insert(task.clone()).await?;
    tokio::spawn(async move {
        task.state = TaskState::Scraping;
        update_task_in_registry(task.clone(), app.registry.as_ref(), app.expire.lock().await).await;

        let doc = match app.scraper.get_markdown_uncropped(input.url).await {
            Ok(doc) => doc,
            Err(err) => {
                task.state = TaskState::Failed(format!("Page scraping failed: {err}").into());
                update_task_in_registry(task, app.registry.as_ref(), app.expire.lock().await).await;
                return;
            }
        };

        let title = doc.title.map(|it| it.to_smolstr());
        task.state = TaskState::Summarizing {
            title: title.clone(),
        };
        update_task_in_registry(task.clone(), app.registry.as_ref(), app.expire.lock().await).await;
        let summary = app.summarizer.summarize(&doc.content).await;
        match summary {
            Ok(summary) => {
                task.state = TaskState::Done {
                    title,
                    summary: summary.into(),
                };
                if let Some(client) = app.apn_client.clone()
                    && let Some(device_token) = input.apn_device_token
                    && let Some(topic) = input.apn_topic
                {
                    tracing::trace!("push APN");
                    _ = push::apn_state_change(&task, &client, &device_token, Some(&topic))
                        .await
                        .inspect_err(|err| {
                            tracing::error!({ topic = topic, device_token = device_token }, "unable to push APN: {err}")
                        });
                } else {
                    tracing::trace!("no APN configured, skipping");
                }
            }
            Err(err) => {
                task.state = TaskState::Failed(format!("Summarization failed: {err}").into());
            }
        }
        update_task_in_registry(task, app.registry.as_ref(), app.expire.lock().await).await;
    });
    Ok(CreateTaskSuccess {
        task_id: task_id.clone(),
    })
}

async fn update_task_in_registry<R>(
    task: Task,
    registry: &R,
    mut expire: impl DerefMut<Target = Expire<R>>,
) where
    R: TaskRegistry + Sync + 'static,
    R::Error: Display + Send + 'static,
{
    expire.deref_mut().insert(task.id).await;
    _ = registry
        .insert(task)
        .await
        .inspect_err(|err| tracing::error!("failed to update task state: {err}"));
}

async fn task_get_handler<R>(
    State(app): State<Arc<AppState<R>>>,
    Path(id): Path<Uuid>,
) -> Result<GetTask, R::Error>
where
    R: TaskRegistry + Sync + 'static,
    R::Error: IntoResponse + Send,
{
    let task = app.registry.get(&id).await?;
    if let Some(task) = task.as_ref() {
        app.expire.lock().await.insert(task.id).await;
    }
    Ok(task.into())
}

async fn task_delete_handler<R>(
    State(app): State<Arc<AppState<R>>>,
    Path(id): Path<Uuid>,
) -> Result<DeleteTask, R::Error>
where
    R: TaskRegistry + Sync + 'static,
    R::Error: IntoResponse + Sync + Send,
{
    let task = app.registry.remove(&id).await?;
    if let Some(task) = task.as_ref() {
        app.expire.lock().await.remove(&task.id).await;
    }
    Ok(task.into())
}

async fn task_sse_get_handler<R>(
    State(app): State<Arc<AppState<R>>>,
    Path(id): Path<Uuid>,
) -> Sse<impl Stream<Item = Result<sse::Event, Infallible>>>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Send + 'static,
{
    let current = app.registry.get(&id).await;
    if let Ok(Some(task)) = current.as_ref() {
        app.expire.lock().await.insert(task.id).await;
    }
    let stream = futures::stream::once(future::ready(current))
        .filter_map(async |it| it.ok().flatten())
        .chain(app.registry.changes_on(id))
        .then(async |task| {
            sse::Event::default()
                .event("update")
                .json_data(task)
                .unwrap()
        })
        .map(Ok);

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
}

#[derive(Debug, Deserialize)]
struct CreateTask {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    apn_device_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apn_topic: Option<String>,
}

struct CreateTaskSuccess {
    task_id: Uuid,
}

enum GetTask {
    NotFound,
    Found(Task),
}

enum DeleteTask {
    NotFound,
    Deleted,
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

impl IntoResponse for DeleteTask {
    fn into_response(self) -> Response {
        match self {
            DeleteTask::NotFound => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("Task not found".into())
                .unwrap(),
            DeleteTask::Deleted => Response::builder()
                .status(StatusCode::OK)
                .body("Task deleted".into())
                .unwrap(),
        }
    }
}

impl From<Option<Task>> for GetTask {
    fn from(value: Option<Task>) -> Self {
        value.map(Self::Found).unwrap_or(Self::NotFound)
    }
}

impl From<Option<Task>> for DeleteTask {
    fn from(value: Option<Task>) -> Self {
        match value {
            Some(_) => Self::Deleted,
            None => Self::NotFound,
        }
    }
}

trait Shutdown {
    async fn shutdown(&self);
}

async fn shutdown_handler(state: impl Shutdown) {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutting down gracefully");
    state.shutdown().await;
}

impl<R> Shutdown for Arc<AppState<CachedRegistry<R>>>
where
    R: TaskRegistry + Send + Sync,
    R::Error: Display + Debug + Send + Sync,
{
    async fn shutdown(&self) {
        match tokio::time::timeout(Duration::from_secs(5), self.registry.flush()).await {
            Ok(_) => {}
            Err(_) => {
                tracing::warn!("failed to flush registry in time, skipping");
            }
        }
    }
}
