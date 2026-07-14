use std::{
    any::Any,
    borrow::Cow,
    convert::Infallible,
    fmt::{Debug, Display},
    future,
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
use axum::{
    Form, Json, RequestExt, Router,
    extract::{FromRequest, Path, Request, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response, Sse, sse},
    routing::{delete, get, patch, post},
};
use clap::{CommandFactory, Parser};
use futures::{Stream, StreamExt};
use rlamus_core::{
    embeddings::{self, Embeddings},
    environ,
    ollama::OllamaRunner,
    scraper::{
        Scraper,
        chromiumoxide::{BrowserConfig, handler::viewport::Viewport},
        compatiblity::CompatibilityLayer,
        youtube::YouTubeSiteScraper,
    },
    summarize::Summarize,
};
use serde::{Deserialize, Serialize};
use smol_str::{SmolStr, ToSmolStr};
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::{
    args::Args,
    shutdown::shutdown_handler,
    state::{AppFoundation, AppState},
    task::{
        CachedRegistry, FsRegistry, Task, TaskRegistry, TaskState,
        expire::{Expire, WithTaskId},
    },
};

mod args;
mod push;
mod shutdown;
mod state;
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

    let llm_ollama = OllamaRunner::default();
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
        llm_ollama.clone(),
        llm_ollama.with_model_from_env_or("EMBEDDING_MODEL", "hf.co/jinaai/jina-embeddings-v5-text-small-clustering:Q4_K_M"),
        args.apn,
        YouTubeSiteScraper::new(args.data_dir.join("youtube"))
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

async fn init<R>(
    tasks: R,
    browser: BrowserConfig,
    llm: OllamaRunner,
    embedding: OllamaRunner,
    apn: args::Apn,
    yt_scraper: YouTubeSiteScraper,
) -> anyhow::Result<(Router, Arc<AppState<R>>)>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Send + Sync + std::error::Error + WithTaskId,
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
    let mut expire = Expire::new(registry.clone(), Duration::from_secs(60 * 60 * 24));

    if let Err(err) = expire.start().await {
        tracing::error!("failed to start expirer: {err}");
    }

    let state = Arc::new(AppState {
        registry: Arc::clone(&registry),
        handles: Default::default(),
        // tasks expire in 1 day
        expire: Mutex::new(expire),
    });
    let foundation = Arc::new(AppFoundation {
        scraper: Scraper::launch_browser(browser, llm.clone())
            .await?
            .compatibility_layer(CompatibilityLayer::default().with_site_scraper(yt_scraper)),
        summarizer: Summarize::new(llm),
        embedder: Embeddings::new(embedding),
        apn_client: apn_client,
    });
    Ok((
        Router::new()
            .route("/", get(root_handler))
            .route("/task", post(task_create_handler))
            .route("/task/{id}", get(task_get_handler))
            .route("/task/{id}", delete(task_delete_handler))
            .route("/task/{id}", patch(task_retry_handler))
            .route("/task/{id}/sse", get(task_sse_get_handler))
            .route("/embeddings", post(get_embeddings_handler))
            .with_state((Arc::clone(&state), Arc::clone(&foundation))),
        state,
    ))
}

async fn run_task<F>(input: CreateTask, app: Arc<AppFoundation>, set_state: F)
where
    F: AsyncFn(TaskState) -> (),
{
    set_state(TaskState::Scraping).await;

    let doc = match app.scraper.get_markdown_uncropped(input.url).await {
        Ok(doc) => doc,
        Err(err) => {
            set_state(TaskState::Failed(
                format!("Page scraping failed: {err}").into(),
            ))
            .await;
            return;
        }
    };

    let title = doc.title.map(|it| it.to_smolstr());
    set_state(TaskState::Summarizing {
        title: title.clone(),
    })
    .await;
    let summary = match app.summarizer.summarize(&doc.content).await {
        Ok(summary) => {
            let ss = summary.to_smolstr();
            set_state(TaskState::Embedding {
                title: title.clone(),
                summary: ss.clone(),
            })
            .await;
            ss
        }
        Err(err) => {
            set_state(TaskState::Failed(
                format!("Summarization failed: {err}").into(),
            ))
            .await;
            return;
        }
    };

    match app.embedder.get_embeddings([summary.clone()]).await {
        Ok(response) => {
            set_state(TaskState::Done {
                title: title.clone(),
                summary: summary.clone(),
                embedding: response.embeddings.into_iter().next().unwrap(),
                embedding_model: response.model_name,
            })
            .await;
        }
        Err(err) => {
            set_state(TaskState::Failed(format!("Embedding failed: {err}").into())).await;
            return;
        }
    }
}

async fn update_task_state<R>(
    task: Task,
    input: &CreateTask,
    state: &AppState<R>,
    foundation: &AppFoundation,
) where
    R: TaskRegistry + Sync + 'static,
    R::Error: Display + Send + 'static,
{
    state.expire.lock().await.insert(task.id).await;
    if let Some(client) = foundation.apn_client.as_ref()
        && let Some(device_token) = input.apn_device_token.as_ref()
        && let Some(topic) = input.apn_topic.as_ref()
    {
        tracing::trace!("push APN");
        _ = push::apn_state_change(&task, client, device_token, Some(topic))
                        .await
                        .inspect_err(|err| {
                            tracing::error!({ topic = ?topic, device_token = ?device_token }, "unable to push APN: {err}")
                        });
    } else {
        tracing::trace!("no APN configured, skipping");
    }

    _ = state
        .registry
        .insert(task)
        .await
        .inspect_err(|err| tracing::error!("failed to update task state: {err}"));
}

// ---- Handlers ----

async fn root_handler() -> &'static str {
    "rlamus-server api:1"
}

async fn task_create_handler<R>(
    State((app, foundation)): State<(Arc<AppState<R>>, Arc<AppFoundation>)>,
    JsonOrForm(input): JsonOrForm<CreateTask>,
) -> Result<CreateTaskSuccess, R::Error>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Display + Send,
{
    let task = Task::new(input.url.clone());
    let task_id = task.id.clone();
    app.registry.insert(task).await?;

    let handle = {
        let app = Arc::clone(&app);
        tokio::spawn(run_task(
            input.clone(),
            Arc::clone(&foundation),
            async move |state| {
                update_task_state(
                    Task {
                        id: task_id.clone(),
                        url: input.url.clone().into(),
                        state,
                    },
                    &input,
                    app.as_ref(),
                    foundation.as_ref(),
                )
                .await
            },
        ))
    };
    app.handles
        .write()
        .await
        .insert(task_id.clone(), AbortOnDropHandle::new(handle));

    Ok(CreateTaskSuccess { task_id })
}

async fn task_retry_handler<R>(
    State((app, foundation)): State<(Arc<AppState<R>>, Arc<AppFoundation>)>,
    Path(id): Path<Uuid>,
    JsonOrForm(input): JsonOrForm<PatchTask>,
) -> Result<PatchTaskResponse, R::Error>
where
    R: TaskRegistry + Send + Sync + 'static,
    R::Error: IntoResponse + Display + Send,
{
    let mut task = app.registry.get(&id).await?;
    if let Some(task) = task.as_mut() {
        if let Some(url) = input.url {
            task.url = url.clone().into();
        }
        task.state = TaskState::Init;
        app.registry.insert(task.clone()).await?;

        let task_url = task.url.clone();
        let create_input = CreateTask {
            url: task_url.clone().into(),
            apn_device_token: input.apn_device_token,
            apn_topic: input.apn_topic,
        };
        let handle = {
            let app = Arc::clone(&app);
            tokio::spawn(run_task(
                create_input.clone(),
                Arc::clone(&foundation),
                async move |state| {
                    update_task_state(
                        Task {
                            id: id.clone(),
                            url: task_url.clone(),
                            state,
                        },
                        &create_input,
                        app.as_ref(),
                        foundation.as_ref(),
                    )
                    .await
                },
            ))
        };
        app.handles
            .write()
            .await
            .insert(task.id, AbortOnDropHandle::new(handle));
    }
    Ok(task.into())
}
async fn task_get_handler<R>(
    State((app, _)): State<(Arc<AppState<R>>, Arc<AppFoundation>)>,
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
    State((app, _)): State<(Arc<AppState<R>>, Arc<AppFoundation>)>,
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
    State((app, _)): State<(Arc<AppState<R>>, Arc<AppFoundation>)>,
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
        .filter_map(async |it| it.ok())
        .chain(app.registry.changes_on(id))
        .then(async |change| {
            if let Some(task) = change {
                sse::Event::default()
                    .event("update")
                    .json_data(task)
                    .unwrap()
            } else {
                sse::Event::default().event("update").data("null")
            }
        })
        .map(Ok);

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
}

async fn get_embeddings_handler<S>(
    State((_, foundation)): State<(S, Arc<AppFoundation>)>,
    JsonOrForm(input): JsonOrForm<GetEmbeddings>,
) -> Result<Json<GetEmbeddingsSuccess>, InternalServerError> {
    let response = foundation.embedder.get_embeddings(input.queries).await?;
    Ok(Json(response.into()))
}

#[derive(Debug, Deserialize, Clone)]
struct CreateTask {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    apn_device_token: Option<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apn_topic: Option<SmolStr>,
}

#[derive(Debug, Deserialize, Clone)]
struct PatchTask {
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apn_device_token: Option<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apn_topic: Option<SmolStr>,
}

#[derive(Debug, Deserialize, Clone)]
struct GetEmbeddings {
    queries: Vec<String>,
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

enum PatchTaskResponse {
    NotFound,
    Patched,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GetEmbeddingsSuccess {
    embeddings: Vec<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_name: Option<String>,
}

struct InternalServerError(anyhow::Error);

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

impl IntoResponse for PatchTaskResponse {
    fn into_response(self) -> Response {
        match self {
            PatchTaskResponse::NotFound => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("Task not found".into())
                .unwrap(),
            PatchTaskResponse::Patched => Response::builder()
                .status(StatusCode::ACCEPTED)
                .body("Task patched".into())
                .unwrap(),
        }
    }
}

impl IntoResponse for InternalServerError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
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

impl From<Option<Task>> for PatchTaskResponse {
    fn from(value: Option<Task>) -> Self {
        match value {
            Some(_) => Self::Patched,
            None => Self::NotFound,
        }
    }
}

impl<E> From<E> for InternalServerError
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(value: E) -> Self {
        Self(anyhow::Error::new(value))
    }
}

impl From<embeddings::Response> for GetEmbeddingsSuccess {
    fn from(value: embeddings::Response) -> Self {
        Self {
            embeddings: value.embeddings,
            model_name: Some(value.model_name),
        }
    }
}

struct JsonOrForm<T>(T);

impl<S, T> FromRequest<S> for JsonOrForm<T>
where
    S: Send + Sync,
    Json<T>: FromRequest<()>,
    Form<T>: FromRequest<()>,
    T: 'static,
{
    type Rejection = Response;

    async fn from_request(req: Request, _state: &S) -> Result<Self, Self::Rejection> {
        let content_type_header = req.headers().get(CONTENT_TYPE);
        let content_type = content_type_header.and_then(|value| value.to_str().ok());

        if let Some(content_type) = content_type {
            if content_type.starts_with("application/json") {
                let Json(payload) = req.extract().await.map_err(IntoResponse::into_response)?;
                return Ok(Self(payload));
            }

            if content_type.starts_with("application/x-www-form-urlencoded") {
                let Form(payload) = req.extract().await.map_err(IntoResponse::into_response)?;
                return Ok(Self(payload));
            }
        }

        Err(StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response())
    }
}
