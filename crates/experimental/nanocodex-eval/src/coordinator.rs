//! Narrow pull coordinator for durable evaluation workers.

use std::{
    collections::HashMap,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::{
    CoordinateClaim, Evaluation, EvaluationClaim, EvaluationSelector, EvaluationTreatment,
    api::EvalApi, cluster::HostSampler,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use futures_util::StreamExt as _;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt as _, net::TcpListener, sync::Mutex};
use tokio_util::io::{ReaderStream, SyncIoBridge};

const MAX_COMPRESSED_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_COMPRESSED_TASK_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_EXTRACTED_TASK_PACKAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const ARCHIVE_BUFFER_BYTES: usize = 64 * 1024;
const ARCHIVE_CONTENT_TYPE: &str = "application/x-tar+zstd";
const EVIDENCE_EXTENSIONS: [&str; 2] = ["json", "jsonl"];
const VERIFIER_EVIDENCE: [&str; 3] = ["reward.txt", "test-stdout.txt", "test-stderr.txt"];
const EXCLUDED_EVIDENCE_DIRECTORIES: [&str; 3] = ["tests", "vm", "workspace"];

/// Loopback HTTP coordinator backed by one durable evaluation ledger.
pub struct CoordinatorServer {
    state: CoordinatorState,
}

/// HTTP client used by one pull worker.
#[derive(Clone, Debug)]
pub struct CoordinatorClient {
    base: Url,
    http: reqwest::Client,
    profile: Option<String>,
    worker: Option<String>,
    write_token: Option<String>,
}

/// One action atomically allocated by the coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteClaim {
    /// Execute one coordinator-allocated repetition.
    Run {
        /// Opaque claim capability required for all later mutations.
        claim: RemoteTaskClaim,
        /// Internal fungible repetition selected by SQLite.
        repetition: u16,
        /// Stable family identity selected by SQLite.
        family_key: String,
        /// Task package selector retained in SQLite.
        task: String,
        /// Immutable source used to materialize the task package.
        task_source: RemoteTaskSource,
        /// Exact treatment retained in SQLite.
        treatment: EvaluationTreatment,
    },
    /// Matching work is temporarily unavailable.
    Busy {
        /// Stable retry classification.
        reason: String,
        /// Coordinator-suggested delay.
        retry_after_ms: u64,
    },
    /// Every desired repetition in this family is terminal.
    Complete,
}

/// Immutable source for one coordinator-owned task package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteTaskSource {
    /// Local coordinator package used only by the loopback development server.
    Filesystem {
        /// Canonical local package root.
        root: PathBuf,
        /// Content digest retained with the workset.
        digest: String,
    },
    /// R2 object owned by the Cloudflare coordinator.
    Package {
        /// Immutable R2 object key.
        key: String,
        /// Expected task content digest.
        digest: String,
    },
}

/// Opaque capability for one running task row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteTaskClaim {
    token: String,
}

/// Coordinator transport or lifecycle failure.
#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    /// Coordinator URL is invalid or unsafe for the initial transport.
    #[error("invalid evaluation coordinator URL: {0}")]
    InvalidUrl(String),
    /// HTTP request failed.
    #[error("evaluation coordinator request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// Local artifact I/O failed.
    #[error("evaluation coordinator artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Coordinator rejected a request.
    #[error("evaluation coordinator rejected the request ({status}): {message}")]
    Rejected {
        /// HTTP status returned by the coordinator.
        status: StatusCode,
        /// Bounded coordinator diagnostic.
        message: String,
    },
    /// A worker attempted to upload evidence outside its output directory.
    #[error("accepted evidence is outside the worker output directory")]
    EvidencePath,
    /// Blocking archive construction or extraction failed.
    #[error("evaluation artifact archive task failed: {0}")]
    ArchiveTask(#[from] tokio::task::JoinError),
    /// Coordinator returned an invalid retained treatment.
    #[error("evaluation coordinator returned an invalid treatment: {0}")]
    InvalidTreatment(String),
    /// Coordinator returned an absent or ambiguous task package source.
    #[error("evaluation coordinator returned an invalid task package source")]
    InvalidTaskSource,
    /// Retained evidence could not be projected into the public case record.
    #[error("evaluation coordinator evidence failed: {0}")]
    Evidence(String),
    /// Durable evaluation ledger could not be recovered.
    #[error("evaluation coordinator ledger failed: {0}")]
    Ledger(#[source] crate::EvaluationError),
}

#[derive(Clone)]
struct CoordinatorState {
    evaluation: Evaluation,
    eval_api: EvalApi,
    active: Arc<Mutex<HashMap<String, ActiveClaim>>>,
    host: Arc<std::sync::Mutex<HostSampler>>,
    ledger_writes: Arc<Mutex<()>>,
}

struct ActiveClaim {
    claim: CoordinateClaim,
    worker: String,
}

#[derive(Deserialize)]
struct ClaimRequest {
    profile: Option<String>,
    task: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    worker: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct WireTreatment {
    harness: String,
    model: String,
    thinking: String,
    web_search: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ClaimResponse {
    Run {
        claim: String,
        repetition: u16,
        family_key: String,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_root: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_package: Option<String>,
        task_digest: String,
        treatment: WireTreatment,
    },
    Busy {
        reason: String,
        retry_after_ms: u64,
    },
    Complete,
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum FinishRequest {
    Success {
        evidence: String,
    },
    Failed {
        error: String,
        evidence: Option<String>,
    },
    Retry {
        error: String,
        evidence: Option<String>,
    },
}

#[derive(Deserialize)]
struct WorkerExitRequest {
    worker: WorkerName,
    error: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkerName {
    String(String),
    Integer(u64),
}

impl WorkerName {
    fn into_string(self) -> String {
        match self {
            Self::String(worker) => worker,
            Self::Integer(worker) => worker.to_string(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
struct EvalOutcomesQuery {
    cursor: Option<usize>,
    limit: Option<usize>,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl CoordinatorServer {
    /// Creates a coordinator whose workers explicitly report terminal outcomes.
    #[must_use]
    pub fn new(evaluation: Evaluation) -> Self {
        let eval_api = EvalApi::new(evaluation.state_directory());
        Self {
            state: CoordinatorState {
                evaluation,
                eval_api,
                active: Arc::new(Mutex::new(HashMap::new())),
                host: Arc::new(std::sync::Mutex::new(HostSampler::new())),
                ledger_writes: Arc::new(Mutex::new(())),
            },
        }
    }

    /// Serves the coordinator on a loopback listener until shutdown.
    pub async fn serve(self, listener: TcpListener) -> Result<(), CoordinatorError> {
        let bind = listener.local_addr()?.ip();
        if !bind.is_loopback() {
            return Err(CoordinatorError::InvalidUrl(
                "coordinators may bind only to loopback".to_owned(),
            ));
        }
        let active = self.state.active.clone();
        let recovered = self
            .state
            .evaluation
            .recover_running()
            .map_err(CoordinatorError::Ledger)?;
        {
            let mut active = active.lock().await;
            for (claim, worker) in recovered {
                active.insert(claim.id().to_owned(), ActiveClaim { claim, worker });
            }
        }
        let app = Router::new()
            .route("/v1/status", get(status))
            .route("/v1/evals", get(eval_overview))
            .route("/v1/evals/cluster", get(eval_cluster))
            .route("/v1/evals/worksets/{digest}", get(eval_workset))
            .route(
                "/v1/evals/worksets/{digest}/analytics",
                get(eval_workset_analytics),
            )
            .route(
                "/v1/evals/worksets/{digest}/tasks/{task_id}",
                get(eval_task),
            )
            .route(
                "/v1/evals/worksets/{digest}/tasks/{task_id}/results",
                get(eval_task_results),
            )
            .route(
                "/v1/evals/worksets/{digest}/tasks/{task_id}/outcomes",
                get(eval_task_outcomes),
            )
            .route("/v1/evals/cases/{id}", get(eval_case))
            .route("/v1/claims", post(claim))
            .route("/v1/claims/{token}/artifacts", put(upload_artifacts))
            .route("/v1/claims/{token}/finish", post(finish))
            .route("/v1/workers/exited", post(worker_exited))
            .with_state(self.state);
        let result = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
        active.lock().await.clear();
        result.map_err(CoordinatorError::Io)
    }
}

impl CoordinatorClient {
    /// Connects to a coordinator. Plain HTTP is accepted only on loopback.
    pub fn new(base: &str) -> Result<Self, CoordinatorError> {
        let mut base =
            Url::parse(base).map_err(|error| CoordinatorError::InvalidUrl(error.to_string()))?;
        let secure = base.scheme() == "https";
        let local_transport = base
            .host_str()
            .and_then(|host| {
                host.trim_matches(|character| character == '[' || character == ']')
                    .parse::<IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback())
            || base.host_str() == Some("localhost");
        if !secure && !(base.scheme() == "http" && local_transport) {
            return Err(CoordinatorError::InvalidUrl(
                "use HTTPS, or HTTP with a loopback address".to_owned(),
            ));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        Ok(Self {
            base,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("nanocodex-eval-worker/", env!("CARGO_PKG_VERSION")))
                .build()?,
            profile: None,
            worker: None,
            write_token: std::env::var("NANOCODEX_EVALS_WRITE_TOKEN")
                .ok()
                .filter(|token| !token.trim().is_empty()),
        })
    }

    /// Restricts status and claims to the newest ready revision of one profile.
    #[must_use]
    pub fn profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }

    /// Attaches an advisory worker name to future claims.
    ///
    /// Names provide observability only. The opaque claim token remains the
    /// authority for the terminal state transition.
    #[must_use]
    pub fn worker(mut self, name: impl Into<String>) -> Self {
        self.worker = Some(name.into());
        self
    }

    /// Uses one bearer credential for every coordinator mutation.
    #[must_use]
    pub fn write_token(mut self, token: impl Into<String>) -> Self {
        self.write_token = Some(token.into());
        self
    }

    /// Reads the coordinator's complete structured ledger snapshot.
    pub async fn status(&self) -> Result<serde_json::Value, CoordinatorError> {
        let mut endpoint = self.endpoint("v1/status")?;
        if let Some(profile) = &self.profile {
            endpoint.query_pairs_mut().append_pair("profile", profile);
        }
        let response = self.http.get(endpoint).send().await?;
        decode(response).await
    }

    /// Downloads and safely materializes one immutable coordinator-owned task package.
    pub async fn materialize_task_package(
        &self,
        key: &str,
        output: &Path,
    ) -> Result<(), CoordinatorError> {
        let mut endpoint = self.endpoint("v1/task-package")?;
        endpoint.query_pairs_mut().append_pair("key", key);
        let response = self.http.get(endpoint).send().await?;
        if !response.status().is_success() {
            return Err(rejected(response).await);
        }
        let parent = output
            .parent()
            .ok_or_else(|| std::io::Error::other("task package output has no parent"))?;
        tokio::fs::create_dir_all(parent).await?;
        let archive = parent.join(".task-package.tar.zst");
        let staging = parent.join(".task-package.staging");
        let mut file = tokio::fs::File::create(&archive).await?;
        let mut stream = response.bytes_stream();
        let mut received = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            received = received
                .checked_add(u64::try_from(chunk.len()).map_err(std::io::Error::other)?)
                .ok_or_else(|| std::io::Error::other("task package is too large"))?;
            if received > MAX_COMPRESSED_TASK_PACKAGE_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&archive).await;
                return Err(std::io::Error::other("task package is too large").into());
            }
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        drop(file);
        let archive_for_task = archive.clone();
        let staging_for_task = staging.clone();
        let output = output.to_path_buf();
        let extraction = tokio::task::spawn_blocking(move || {
            extract_task_package_archive(&archive_for_task, &staging_for_task, &output)
        })
        .await?;
        let _ = tokio::fs::remove_file(&archive).await;
        if let Err(error) = extraction {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(error.into());
        }
        Ok(())
    }

    /// Claims one pre-materialized task row matching ordinary SQLite selectors.
    pub async fn claim(
        &self,
        selection: &EvaluationSelector,
    ) -> Result<RemoteClaim, CoordinatorError> {
        let response: ClaimResponse = decode(
            self.authorize(self.http.post(self.endpoint("v1/claims")?))
                .json(&serde_json::json!({
                    "profile": self.profile,
                    "task": selection.task(),
                    "harness": selection.harness_name(),
                    "model": selection.model_name(),
                    "thinking": selection.thinking_name(),
                    "worker": self.worker,
                }))
                .send()
                .await?,
        )
        .await?;
        Ok(match response {
            ClaimResponse::Run {
                claim,
                repetition,
                family_key,
                task,
                task_root,
                task_package,
                task_digest,
                treatment,
            } => RemoteClaim::Run {
                claim: RemoteTaskClaim { token: claim },
                repetition,
                family_key,
                task,
                task_source: remote_task_source(task_root, task_package, task_digest)?,
                treatment: treatment.try_into()?,
            },
            ClaimResponse::Busy {
                reason,
                retry_after_ms,
            } => RemoteClaim::Busy {
                reason,
                retry_after_ms,
            },
            ClaimResponse::Complete => RemoteClaim::Complete,
        })
    }

    /// Claims the next unclaimed row without selecting a task or treatment.
    pub async fn claim_next(&self) -> Result<RemoteClaim, CoordinatorError> {
        let response: ClaimResponse = decode(
            self.authorize(self.http.post(self.endpoint("v1/claims")?))
                .json(&serde_json::json!({
                    "profile": self.profile,
                    "worker": self.worker,
                }))
                .send()
                .await?,
        )
        .await?;
        Ok(match response {
            ClaimResponse::Run {
                claim,
                repetition,
                family_key,
                task,
                task_root,
                task_package,
                task_digest,
                treatment,
            } => RemoteClaim::Run {
                claim: RemoteTaskClaim { token: claim },
                repetition,
                family_key,
                task,
                task_source: remote_task_source(task_root, task_package, task_digest)?,
                treatment: treatment.try_into()?,
            },
            ClaimResponse::Busy {
                reason,
                retry_after_ms,
            } => RemoteClaim::Busy {
                reason,
                retry_after_ms,
            },
            ClaimResponse::Complete => RemoteClaim::Complete,
        })
    }

    /// Reports that this named worker process exited before recording an outcome.
    ///
    /// The operation is idempotent. If the worker already recorded a terminal
    /// result or never claimed a row, the coordinator has nothing to change.
    pub async fn worker_exited(&self, error: &str) -> Result<(), CoordinatorError> {
        let worker = self
            .worker
            .as_deref()
            .ok_or_else(|| CoordinatorError::Rejected {
                status: StatusCode::BAD_REQUEST,
                message: "worker exit reports require a configured worker name".to_owned(),
            })?;
        accepted(
            self.authorize(self.http.post(self.endpoint("v1/workers/exited")?))
                .json(&serde_json::json!({ "worker": worker, "error": error }))
                .send()
                .await?,
        )
        .await
    }

    /// Extends one active claim lease while its worker is still making progress.
    pub async fn heartbeat(&self, claim: &RemoteTaskClaim) -> Result<(), CoordinatorError> {
        accepted(
            self.authorize(
                self.http
                    .post(self.endpoint(&format!("v1/claims/{}/heartbeat", claim.token))?),
            )
            .send()
            .await?,
        )
        .await
    }

    /// Records a verifier-failing result for one running task row.
    pub async fn fail(&self, claim: &RemoteTaskClaim, error: &str) -> Result<(), CoordinatorError> {
        self.finish(
            claim,
            serde_json::json!({ "outcome": "failed", "error": error }),
        )
        .await
    }

    /// Uploads retained evidence and records a verifier-failing result.
    pub async fn fail_with_evidence(
        &self,
        claim: &RemoteTaskClaim,
        output_directory: &Path,
        evidence: &Path,
        error: &str,
    ) -> Result<(), CoordinatorError> {
        let evidence = evidence
            .strip_prefix(output_directory)
            .map_err(|_| CoordinatorError::EvidencePath)?;
        let case = EvalApi::evidence(output_directory).map_err(CoordinatorError::Evidence)?;
        self.upload(claim, output_directory).await?;
        self.finish(
            claim,
            serde_json::json!({
                "outcome": "failed",
                "error": error,
                "evidence": evidence.to_string_lossy(),
                "case": case,
            }),
        )
        .await
    }

    /// Records an infrastructure-failed attempt and makes its row claimable again.
    pub async fn retry(
        &self,
        claim: &RemoteTaskClaim,
        error: &str,
    ) -> Result<(), CoordinatorError> {
        self.finish(
            claim,
            serde_json::json!({ "outcome": "retry", "error": error }),
        )
        .await
    }

    /// Uploads infrastructure-failure evidence before making the row claimable again.
    pub async fn retry_with_evidence(
        &self,
        claim: &RemoteTaskClaim,
        output_directory: &Path,
        evidence: &Path,
        error: &str,
    ) -> Result<(), CoordinatorError> {
        let evidence = evidence
            .strip_prefix(output_directory)
            .map_err(|_| CoordinatorError::EvidencePath)?;
        let case = EvalApi::evidence(output_directory).map_err(CoordinatorError::Evidence)?;
        self.upload(claim, output_directory).await?;
        self.finish(
            claim,
            serde_json::json!({
                "outcome": "retry",
                "error": error,
                "evidence": evidence.to_string_lossy(),
                "case": case,
            }),
        )
        .await
    }

    /// Uploads canonical attempt evidence and records terminal success.
    pub async fn succeed(
        &self,
        claim: &RemoteTaskClaim,
        output_directory: &Path,
        evidence: &Path,
    ) -> Result<(), CoordinatorError> {
        let evidence = evidence
            .strip_prefix(output_directory)
            .map_err(|_| CoordinatorError::EvidencePath)?;
        let case = EvalApi::evidence(output_directory).map_err(CoordinatorError::Evidence)?;
        self.upload(claim, output_directory).await?;
        self.finish(
            claim,
            serde_json::json!({
                "outcome": "success",
                "evidence": evidence.to_string_lossy(),
                "case": case,
            }),
        )
        .await
    }

    /// Uploads retained canonical evidence before recording a terminal result.
    pub async fn upload(
        &self,
        claim: &RemoteTaskClaim,
        output_directory: &Path,
    ) -> Result<(), CoordinatorError> {
        let (writer, reader) = tokio::io::duplex(ARCHIVE_BUFFER_BYTES);
        let directory = output_directory.to_path_buf();
        let archive = tokio::task::spawn_blocking(move || {
            write_evidence_archive(&directory, SyncIoBridge::new(writer))
        });
        let response = self
            .authorize(
                self.http
                    .put(self.endpoint(&format!("v1/claims/{}/artifacts", claim.token))?),
            )
            .header(reqwest::header::CONTENT_TYPE, ARCHIVE_CONTENT_TYPE)
            .body(reqwest::Body::wrap_stream(ReaderStream::new(reader)))
            .send()
            .await;
        archive.await??;
        accepted(response?).await
    }

    fn endpoint(&self, path: &str) -> Result<Url, CoordinatorError> {
        self.base
            .join(path)
            .map_err(|error| CoordinatorError::InvalidUrl(error.to_string()))
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.write_token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn finish(
        &self,
        claim: &RemoteTaskClaim,
        body: serde_json::Value,
    ) -> Result<(), CoordinatorError> {
        accepted(
            self.authorize(
                self.http
                    .post(self.endpoint(&format!("v1/claims/{}/finish", claim.token))?),
            )
            .json(&body)
            .send()
            .await?,
        )
        .await
    }
}

async fn status(
    State(state): State<CoordinatorState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let evaluation = state.evaluation;
    let status = tokio::task::spawn_blocking(move || evaluation.status())
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::ledger)?;
    Ok(Json(
        serde_json::to_value(status).map_err(ApiError::internal)?,
    ))
}

async fn eval_overview(State(state): State<CoordinatorState>) -> Result<Response, ApiError> {
    let api = state.eval_api;
    let overview = tokio::task::spawn_blocking(move || api.overview())
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(overview).into_response())
}

async fn eval_cluster(State(state): State<CoordinatorState>) -> Result<Response, ApiError> {
    let claimed_tasks = state.active.lock().await.len();
    let host = state.host;
    let snapshot = tokio::task::spawn_blocking(move || {
        host.lock()
            .map_err(|_| "evaluation host sampler lock was poisoned".to_owned())
            .map(|mut host| host.snapshot(claimed_tasks))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    Ok(Json(snapshot).into_response())
}

async fn eval_workset(
    State(state): State<CoordinatorState>,
    AxumPath(digest): AxumPath<String>,
) -> Result<Response, ApiError> {
    let api = state.eval_api;
    let workset = tokio::task::spawn_blocking(move || api.workset(&digest))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    workset
        .map(|workset| Json(workset).into_response())
        .ok_or_else(|| ApiError::not_found("evaluation workset was not found"))
}

async fn eval_workset_analytics(
    State(state): State<CoordinatorState>,
    AxumPath(digest): AxumPath<String>,
) -> Result<Response, ApiError> {
    let api = state.eval_api;
    let analytics = tokio::task::spawn_blocking(move || api.workset_analytics(&digest))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    analytics
        .map(|analytics| Json(analytics).into_response())
        .ok_or_else(|| ApiError::not_found("evaluation workset was not found"))
}

async fn eval_task_results(
    State(state): State<CoordinatorState>,
    AxumPath((digest, task_id)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let api = state.eval_api;
    let results = tokio::task::spawn_blocking(move || api.task_results(&digest, &task_id))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    results
        .map(|results| Json(results).into_response())
        .ok_or_else(|| ApiError::not_found("evaluation task was not found"))
}

async fn eval_case(
    State(state): State<CoordinatorState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let api = state.eval_api;
    let evidence = tokio::task::spawn_blocking(move || api.case(&id))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    evidence
        .map(|evidence| Json(evidence).into_response())
        .ok_or_else(|| ApiError::not_found("evaluation case was not found"))
}

async fn eval_task(
    State(state): State<CoordinatorState>,
    AxumPath((digest, task_id)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let api = state.eval_api;
    let task = tokio::task::spawn_blocking(move || api.task(&digest, &task_id))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    task.map(|task| Json(task).into_response())
        .ok_or_else(|| ApiError::not_found("evaluation task was not found"))
}

async fn eval_task_outcomes(
    State(state): State<CoordinatorState>,
    AxumPath((digest, task_id)): AxumPath<(String, String)>,
    Query(query): Query<EvalOutcomesQuery>,
) -> Result<Response, ApiError> {
    let cursor = query.cursor.unwrap_or(0);
    let limit = query.limit.unwrap_or(8);
    if !(1..=32).contains(&limit) {
        return Err(ApiError::bad_request(
            "evaluation outcome page limit must be between 1 and 32",
        ));
    }
    let api = state.eval_api;
    let page =
        tokio::task::spawn_blocking(move || api.task_outcomes(&digest, &task_id, cursor, limit))
            .await
            .map_err(ApiError::internal)?
            .map_err(ApiError::internal)?;
    page.map(|page| Json(page).into_response())
        .ok_or_else(|| ApiError::not_found("evaluation task was not found"))
}

async fn claim(
    State(state): State<CoordinatorState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<ClaimRequest>,
) -> Result<Json<ClaimResponse>, ApiError> {
    let _profile = request.profile;
    let host = request
        .worker
        .filter(|worker| !worker.trim().is_empty())
        .unwrap_or_else(|| peer.ip().to_string());
    let selector = match request.task {
        Some(task) => {
            let model = request
                .model
                .map(|model| model.parse().map_err(ApiError::bad_request))
                .transpose()?;
            let thinking = request
                .thinking
                .map(|thinking| thinking.parse().map_err(ApiError::bad_request))
                .transpose()?;
            let selector = EvaluationSelector::new(task)
                .harness(request.harness)
                .model(model)
                .thinking(thinking);
            Some(selector)
        }
        None => None,
    };
    let _write = state.ledger_writes.lock().await;
    let evaluation = state.evaluation.clone();
    let claim_host = host.clone();
    let claim = tokio::task::spawn_blocking(move || match selector {
        Some(selector) => evaluation.claim_for_worker(&selector, &claim_host),
        None => evaluation.claim_next_for_worker(&claim_host),
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::bad_gateway)?;
    let response = match claim {
        EvaluationClaim::Run(claim) => {
            let repetition = claim.repetition();
            let family_key = claim.family_key().to_owned();
            let task = claim.task_selector().to_owned();
            let task_root = claim.task().root().to_path_buf();
            let task_digest = claim.task().package_digest().to_owned();
            let treatment = WireTreatment::from(claim.treatment());
            let claim = insert_claim(&state, claim, host).await;
            ClaimResponse::Run {
                claim,
                repetition,
                family_key,
                task,
                task_root: Some(task_root),
                task_package: None,
                task_digest,
                treatment,
            }
        }
        EvaluationClaim::Busy(busy) => ClaimResponse::Busy {
            reason: busy.reason.to_owned(),
            retry_after_ms: busy.retry_after_ms,
        },
        EvaluationClaim::Complete => ClaimResponse::Complete,
    };
    Ok(Json(response))
}

async fn worker_exited(
    State(state): State<CoordinatorState>,
    Json(request): Json<WorkerExitRequest>,
) -> Result<StatusCode, ApiError> {
    let worker = request.worker.into_string();
    if worker.trim().is_empty() {
        return Err(ApiError::bad_request("worker name must not be empty"));
    }
    if request.error.trim().is_empty() {
        return Err(ApiError::bad_request("worker exit error must not be empty"));
    }
    let error = request.error;
    let observed_worker = worker.clone();
    let host = state.host.clone();
    let worker_is_live = tokio::task::spawn_blocking(move || {
        host.lock()
            .map_err(|_| "evaluation host sampler lock was poisoned".to_owned())
            .map(|mut host| host.worker_is_live(&observed_worker))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    if worker_is_live {
        return Err(ApiError::conflict(format!(
            "worker {worker} still has a live eval process"
        )));
    }
    let _write = state.ledger_writes.lock().await;
    let exited = {
        let mut active = state.active.lock().await;
        let tokens = active
            .iter()
            .filter(|(_, claim)| claim.worker == worker)
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        tokens
            .into_iter()
            .filter_map(|token| active.remove(&token))
            .collect::<Vec<_>>()
    };
    tokio::task::spawn_blocking(move || {
        for active in exited {
            active.claim.release(&error).map_err(ApiError::ledger)?;
        }
        Ok::<_, ApiError>(())
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_artifacts(
    State(state): State<CoordinatorState>,
    AxumPath(token): AxumPath<String>,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let output = {
        let active = state.active.lock().await;
        let claim = active
            .get(&token)
            .ok_or_else(|| ApiError::not_found("claim is absent or expired"))?;
        claim.claim.output_directory().to_path_buf()
    };
    receive_archive(body, &output, &token).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn finish(
    State(state): State<CoordinatorState>,
    AxumPath(token): AxumPath<String>,
    Json(request): Json<FinishRequest>,
) -> Result<StatusCode, ApiError> {
    let _write = state.ledger_writes.lock().await;
    let active = state
        .active
        .lock()
        .await
        .remove(&token)
        .ok_or_else(|| ApiError::not_found("claim is absent or expired"))?;
    let eval_api = state.eval_api.clone();
    tokio::task::spawn_blocking(move || finish_claim(active, request, &eval_api))
        .await
        .map_err(ApiError::internal)??;
    Ok(StatusCode::NO_CONTENT)
}

fn finish_claim(
    active: ActiveClaim,
    request: FinishRequest,
    eval_api: &EvalApi,
) -> Result<(), ApiError> {
    match request {
        FinishRequest::Success { evidence } => {
            let claim = active.claim;
            let evidence = safe_evidence(claim.output_directory(), &evidence)?;
            if !evidence.exists() {
                return Err(ApiError::bad_request("accepted evidence was not uploaded"));
            }
            claim.succeed(&evidence).map_err(ApiError::ledger)?;
            if let Err(error) = eval_api.index_result(&evidence) {
                tracing::warn!(%error, "failed to index accepted evaluation result");
            }
        }
        FinishRequest::Failed { error, evidence } => {
            let evidence = evidence
                .as_deref()
                .map(|evidence| safe_evidence(active.claim.output_directory(), evidence))
                .transpose()?;
            if evidence.as_ref().is_some_and(|evidence| !evidence.exists()) {
                return Err(ApiError::bad_request("failure evidence was not uploaded"));
            }
            active
                .claim
                .fail(evidence.as_deref(), &error)
                .map_err(ApiError::ledger)?;
            if let Some(evidence) = evidence
                && let Err(error) = eval_api.index_result(&evidence)
            {
                tracing::warn!(%error, "failed to index accepted evaluation result");
            }
        }
        FinishRequest::Retry { error, evidence } => {
            let evidence = evidence
                .as_deref()
                .map(|evidence| safe_evidence(active.claim.output_directory(), evidence))
                .transpose()?;
            if evidence.as_ref().is_some_and(|evidence| !evidence.exists()) {
                return Err(ApiError::bad_request("failure evidence was not uploaded"));
            }
            active
                .claim
                .retry(evidence.as_deref(), &error)
                .map_err(ApiError::ledger)?;
        }
    }
    Ok(())
}

async fn insert_claim(state: &CoordinatorState, claim: CoordinateClaim, worker: String) -> String {
    let token = claim.id().to_owned();
    state
        .active
        .lock()
        .await
        .insert(token.clone(), ActiveClaim { claim, worker });
    token
}

async fn receive_archive(body: Body, output: &Path, token: &str) -> Result<(), ApiError> {
    let parent = output
        .parent()
        .ok_or_else(|| ApiError::internal("claim output has no parent"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(ApiError::internal)?;
    remove_stale_uploads(parent).await?;
    let upload = parent.join(format!(".{token}.tar.zst"));
    let staging = parent.join(format!(".{token}.staging"));
    let mut file = tokio::fs::File::create(&upload)
        .await
        .map_err(ApiError::internal)?;
    let mut stream = body.into_data_stream();
    let mut received = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ApiError::internal)?;
        received = received
            .checked_add(u64::try_from(chunk.len()).map_err(ApiError::internal)?)
            .ok_or_else(|| ApiError::bad_request("artifact upload is too large"))?;
        if received > MAX_COMPRESSED_ARTIFACT_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&upload).await;
            return Err(ApiError::bad_request("artifact upload is too large"));
        }
        file.write_all(&chunk).await.map_err(ApiError::internal)?;
    }
    file.sync_all().await.map_err(ApiError::internal)?;
    drop(file);
    let upload_for_task = upload.clone();
    let output = output.to_path_buf();
    let staging_for_task = staging.clone();
    let extraction = tokio::task::spawn_blocking(move || {
        extract_evidence_archive(&upload_for_task, &staging_for_task, &output)
    })
    .await
    .map_err(ApiError::internal)?;
    let _ = tokio::fs::remove_file(&upload).await;
    if let Err(error) = extraction {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(ApiError::bad_request(error));
    }
    Ok(())
}

async fn remove_stale_uploads(parent: &Path) -> Result<(), ApiError> {
    let mut entries = tokio::fs::read_dir(parent)
        .await
        .map_err(ApiError::internal)?;
    while let Some(entry) = entries.next_entry().await.map_err(ApiError::internal)? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if name.ends_with(".tar") || name.ends_with(".tar.zst") {
            tokio::fs::remove_file(path)
                .await
                .map_err(ApiError::internal)?;
        } else if name.ends_with(".staging") {
            tokio::fs::remove_dir_all(path)
                .await
                .map_err(ApiError::internal)?;
        }
    }
    Ok(())
}

fn extract_evidence_archive(archive: &Path, staging: &Path, output: &Path) -> std::io::Result<()> {
    std::fs::create_dir(staging)?;
    let file = std::fs::File::open(archive)?;
    let decoder = zstd::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);
    let mut extracted = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !entry.header().entry_type().is_file() || !is_evidence_path(&path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "artifact archive contained unsupported evidence: {}",
                    path.display()
                ),
            ));
        }
        extracted = extracted
            .checked_add(entry.size())
            .ok_or_else(|| std::io::Error::other("extracted evidence is too large"))?;
        if extracted > MAX_EXTRACTED_ARTIFACT_BYTES {
            return Err(std::io::Error::other("extracted evidence is too large"));
        }
        if !entry.unpack_in(staging)? {
            return Err(std::io::Error::other(
                "artifact archive escaped its output directory",
            ));
        }
    }
    match std::fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_dir() => std::fs::remove_dir_all(output)?,
        Ok(_) => {
            return Err(std::io::Error::other(
                "existing artifact output is not a directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::rename(staging, output)?;
    Ok(())
}

fn extract_task_package_archive(
    archive: &Path,
    staging: &Path,
    output: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir(staging)?;
    let file = std::fs::File::open(archive)?;
    let decoder = zstd::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);
    let mut extracted = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let safe_path = !path.as_os_str().is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        let entry_type = entry.header().entry_type();
        if !safe_path || !(entry_type.is_file() || entry_type.is_dir()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("task archive contained an unsafe entry: {}", path.display()),
            ));
        }
        if entry_type.is_file() {
            extracted = extracted
                .checked_add(entry.size())
                .ok_or_else(|| std::io::Error::other("extracted task package is too large"))?;
            if extracted > MAX_EXTRACTED_TASK_PACKAGE_BYTES {
                return Err(std::io::Error::other("extracted task package is too large"));
            }
        }
        if !entry.unpack_in(staging)? {
            return Err(std::io::Error::other(
                "task archive escaped its output directory",
            ));
        }
    }
    if std::fs::symlink_metadata(output).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "task package output already exists",
        ));
    }
    std::fs::rename(staging, output)
}

fn write_evidence_archive<W: Write>(directory: &Path, writer: W) -> std::io::Result<()> {
    let encoder = zstd::Encoder::new(writer, 3)?;
    let mut archive = tar::Builder::new(encoder);
    append_evidence(&mut archive, directory, directory)?;
    let encoder = archive.into_inner()?;
    encoder.finish()?.flush()
}

fn append_evidence<W: Write>(
    archive: &mut tar::Builder<W>,
    root: &Path,
    directory: &Path,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| std::io::Error::other("evidence path escaped its output directory"))?;
        if file_type.is_dir() {
            if !is_excluded_evidence_directory(relative) {
                append_evidence(archive, root, &path)?;
            }
        } else if file_type.is_file() && is_evidence_path(relative) {
            archive.append_path_with_name(&path, relative)?;
        }
    }
    Ok(())
}

fn is_evidence_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !is_excluded_evidence_directory(path)
        && (path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| EVIDENCE_EXTENSIONS.contains(&extension))
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| VERIFIER_EVIDENCE.contains(&name)))
}

fn is_excluded_evidence_directory(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        name.to_str()
            .is_some_and(|name| EXCLUDED_EVIDENCE_DIRECTORIES.contains(&name))
    })
}

fn safe_evidence(output: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(ApiError::bad_request(
            "evidence path escaped its uploaded attempt",
        ));
    }
    Ok(output.join(relative))
}

async fn accepted(response: reqwest::Response) -> Result<(), CoordinatorError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(rejected(response).await)
    }
}

async fn decode<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, CoordinatorError> {
    if response.status().is_success() {
        Ok(response.json().await?)
    } else {
        Err(rejected(response).await)
    }
}

async fn rejected(response: reqwest::Response) -> CoordinatorError {
    let status = response.status();
    let message = response
        .text()
        .await
        .unwrap_or_else(|_| "coordinator response body was unreadable".to_owned());
    CoordinatorError::Rejected {
        status,
        message: message.chars().take(4_096).collect(),
    }
}

impl ApiError {
    fn bad_request(message: impl ToString) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    fn not_found(message: impl ToString) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.to_string(),
        }
    }

    fn bad_gateway(message: impl ToString) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.to_string(),
        }
    }

    fn conflict(message: impl ToString) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.to_string(),
        }
    }

    fn ledger(error: crate::EvaluationError) -> Self {
        Self::bad_gateway(error)
    }

    fn internal(message: impl ToString) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody {
            error: &self.message,
        });
        (self.status, body).into_response()
    }
}

impl From<&EvaluationTreatment> for WireTreatment {
    fn from(treatment: &EvaluationTreatment) -> Self {
        Self {
            harness: treatment.harness.clone(),
            model: treatment.model.as_str().to_owned(),
            thinking: treatment.thinking.as_str().to_owned(),
            web_search: treatment.web_search,
        }
    }
}

fn remote_task_source(
    root: Option<PathBuf>,
    package: Option<String>,
    digest: String,
) -> Result<RemoteTaskSource, CoordinatorError> {
    match (root, package) {
        (Some(root), None) => Ok(RemoteTaskSource::Filesystem { root, digest }),
        (None, Some(key)) if !key.is_empty() => Ok(RemoteTaskSource::Package { key, digest }),
        _ => Err(CoordinatorError::InvalidTaskSource),
    }
}

impl TryFrom<WireTreatment> for EvaluationTreatment {
    type Error = CoordinatorError;

    fn try_from(treatment: WireTreatment) -> Result<Self, Self::Error> {
        let model = treatment
            .model
            .parse()
            .map_err(CoordinatorError::InvalidTreatment)?;
        let thinking = treatment
            .thinking
            .parse()
            .map_err(CoordinatorError::InvalidTreatment)?;
        Ok(Self {
            harness: treatment.harness,
            model,
            thinking,
            web_search: treatment.web_search,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use rusqlite::Connection;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::{
        EvaluationSelector, Task,
        coordinator::RemoteClaim,
        workset::{BeginTask, Workset},
    };

    fn write_task(root: &Path) {
        let task = root.join("one");
        fs::create_dir_all(task.join("environment")).unwrap();
        fs::create_dir_all(task.join("tests")).unwrap();
        fs::write(
            task.join("task.toml"),
            r#"schema_version = "1.1"
[task]
name = "one"
description = "test"
[agent]
timeout_sec = 1.0
[verifier]
timeout_sec = 1.0
[environment]
docker_image = "alpine:3.21"
cpus = 1
memory_mb = 128
storage_mb = 128
gpus = 0
allow_internet = false
"#,
        )
        .unwrap();
        fs::write(task.join("instruction.md"), "do it").unwrap();
        fs::write(task.join("environment/Dockerfile"), "FROM scratch").unwrap();
        fs::write(task.join("tests/test.sh"), "#!/bin/sh\n").unwrap();
    }

    async fn fixture() -> (
        tempfile::TempDir,
        CoordinatorClient,
        EvaluationSelector,
        JoinHandle<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path());
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"[profiles.release]
tasks = ["one"]
trials = 2
model = ["sol"]
thinking = ["high"]
"#,
        )
        .unwrap();
        Evaluation::add_profile(
            &config,
            Some("release"),
            directory.path().join("state"),
            "release",
            false,
        )
        .unwrap();
        let evaluation =
            Evaluation::open(&config, Some("release"), directory.path().join("state")).unwrap();
        let selection = EvaluationSelector::new("one");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            CoordinatorServer::new(evaluation)
                .serve(listener)
                .await
                .unwrap();
        });
        let client = CoordinatorClient::new(&format!("http://{address}"))
            .unwrap()
            .profile("release");
        (directory, client, selection, server)
    }

    #[tokio::test]
    async fn workers_claim_upload_and_converge_through_the_coordinator() {
        let (directory, client, selection, server) = fixture().await;
        let first_client = client.clone().worker("worker-one");
        let second_client = client.clone().worker("worker-two");
        let (first, second) =
            tokio::join!(first_client.claim_next(), second_client.claim(&selection));
        let RemoteClaim::Run {
            claim: first_claim,
            repetition: first_repetition,
            family_key: first_family_key,
            task_source: first_task_source,
            ..
        } = first.unwrap()
        else {
            panic!("first worker should run");
        };
        let RemoteClaim::Run {
            claim: second_claim,
            repetition: second_repetition,
            ..
        } = second.unwrap()
        else {
            panic!("second worker should run");
        };
        assert_ne!(first_repetition, second_repetition);
        let RemoteTaskSource::Filesystem {
            root: first_task_root,
            ..
        } = first_task_source
        else {
            panic!("loopback coordinator should retain a filesystem task source");
        };
        assert_eq!(
            first_task_root,
            fs::canonicalize(directory.path().join("one")).unwrap()
        );

        let status = client.status().await.unwrap();
        let profile_digest = status["digest"].as_str().unwrap();
        let family_digest = {
            use sha2::{Digest as _, Sha256};
            hex::encode(Sha256::digest(first_family_key.as_bytes()))
        };
        let stale_output = directory
            .path()
            .join("state/artifacts")
            .join(profile_digest)
            .join(family_digest)
            .join(format!("k-{first_repetition}"));
        fs::create_dir_all(&stale_output).unwrap();
        let stale_marker = stale_output.join("stale.json");
        fs::write(&stale_marker, "{\"stale\":true}\n").unwrap();

        for (claim, name) in [(&first_claim, "first"), (&second_claim, "second")] {
            let output = directory.path().join(format!("worker-{name}"));
            fs::create_dir_all(output.join("agent")).unwrap();
            fs::create_dir_all(output.join("verifier")).unwrap();
            fs::create_dir_all(output.join("workspace")).unwrap();
            fs::create_dir_all(output.join("tests")).unwrap();
            fs::create_dir_all(output.join("vm")).unwrap();
            let evidence = output.join("comparison.json");
            fs::write(&evidence, format!("{{\"worker\":\"{name}\"}}\n")).unwrap();
            fs::write(
                output.join("events.jsonl"),
                "{\"type\":\"attempt_started\",\"payload\":{\"prompt\":\"do it\"},\"attempt\":{\"task_name\":\"one\"}}\n\
                 {\"type\":\"completed\",\"payload\":{\"task_name\":\"one\",\"status\":\"passed\",\"outcome\":\"passed\",\"environment\":\"micro_vm\",\"agent\":{\"model\":\"sol\",\"effort\":\"high\",\"tool_calls\":1,\"cost_usd\":\"0.042\",\"usage\":{\"input_tokens\":7,\"cached_input_tokens\":3,\"output_tokens\":3,\"reasoning_output_tokens\":2,\"total_tokens\":10}},\"verifier\":{\"exit_code\":0,\"rewards\":{\"reward\":1}}}}\n",
            )
            .unwrap();
            fs::write(
                output.join("agent/trajectory.json"),
                format!("{{\"worker\":\"{name}\"}}\n"),
            )
            .unwrap();
            fs::write(output.join("verifier/reward.txt"), "1\n").unwrap();
            fs::write(output.join("rootfs.ext4"), vec![0_u8; 1024 * 1024]).unwrap();
            fs::write(output.join("workspace/result.json"), "{}\n").unwrap();
            fs::write(output.join("tests/fixture.json"), "{}\n").unwrap();
            fs::write(output.join("vm/config.json"), "{}\n").unwrap();
            client.succeed(claim, &output, &evidence).await.unwrap();
        }
        assert!(
            stale_marker.exists(),
            "one attempt must not erase sibling evidence"
        );

        let status = client.status().await.unwrap();
        assert_eq!(status["tasks"]["success"], 2);
        assert_eq!(status["tasks"]["unclaimed"], 0);
        let overview: serde_json::Value = decode(
            client
                .http
                .get(client.endpoint("v1/evals").unwrap())
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(overview["worksets"][0]["summary"]["success"], 2);
        assert_eq!(overview["worksets"][0]["taskCount"], 1);
        let digest = overview["worksets"][0]["id"].as_str().unwrap();
        let workset: serde_json::Value = decode(
            client
                .http
                .get(
                    client
                        .endpoint(&format!("v1/evals/worksets/{digest}"))
                        .unwrap(),
                )
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(workset["tasks"][0]["summary"]["success"], 2);
        let task_id = workset["tasks"][0]["id"].as_str().unwrap();
        let task: serde_json::Value = decode(
            client
                .http
                .get(
                    client
                        .endpoint(&format!("v1/evals/worksets/{digest}/tasks/{task_id}"))
                        .unwrap(),
                )
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            task["task"]["treatments"][0]["cells"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            task["task"]["treatments"][0]["cells"][0]["status"],
            "passed"
        );
        let analytics: serde_json::Value = decode(
            client
                .http
                .get(
                    client
                        .endpoint(&format!("v1/evals/worksets/{digest}/analytics"))
                        .unwrap(),
                )
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(analytics["taskCount"], 1);
        assert_eq!(analytics["points"].as_array().unwrap().len(), 1);
        assert_eq!(analytics["points"][0]["passed"], 2);
        assert_eq!(analytics["points"][0]["completed"], 2);
        assert_eq!(analytics["points"][0]["medianOutputTokens"], 3.0);
        assert_eq!(analytics["points"][0]["outputSamples"], 2);
        assert_eq!(analytics["points"][0]["medianCostUsd"], 0.042);
        let task_results: serde_json::Value = decode(
            client
                .http
                .get(
                    client
                        .endpoint(&format!(
                            "v1/evals/worksets/{digest}/tasks/{task_id}/results"
                        ))
                        .unwrap(),
                )
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(task_results["points"].as_array().unwrap().len(), 2);
        assert_eq!(
            task["task"]["treatments"][0]["cells"][0]["state"],
            "success"
        );
        let outcomes: serde_json::Value = decode(
            client
                .http
                .get(
                    client
                        .endpoint(&format!(
                            "v1/evals/worksets/{digest}/tasks/{task_id}/outcomes?limit=1"
                        ))
                        .unwrap(),
                )
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(outcomes["total"], 2);
        assert_eq!(outcomes["outcomes"][0]["status"], "passed");
        assert_eq!(outcomes["nextCursor"], 1);
        let case = task["task"]["treatments"][0]["cells"][0]["detailId"]
            .as_str()
            .unwrap();
        let detail: serde_json::Value = decode(
            client
                .http
                .get(client.endpoint(&format!("v1/evals/cases/{case}")).unwrap())
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(detail["status"], "passed");
        assert_eq!(detail["prompt"], "do it");
        let artifacts = directory.path().join("state/artifacts");
        assert_eq!(count_named_files(&artifacts, "events.jsonl"), 2);
        assert_eq!(count_named_files(&artifacts, "trajectory.json"), 2);
        assert_eq!(count_named_files(&artifacts, "reward.txt"), 2);
        assert_eq!(count_named_files(&artifacts, "rootfs.ext4"), 0);
        assert_eq!(count_named_files(&artifacts, "result.json"), 0);
        assert_eq!(count_named_files(&artifacts, "fixture.json"), 0);
        assert_eq!(count_named_files(&artifacts, "config.json"), 0);
        assert_eq!(count_files_with_suffix(&artifacts, ".tar"), 0);
        assert_eq!(count_files_with_suffix(&artifacts, ".tar.zst"), 0);
        assert!(matches!(
            client.claim(&selection).await.unwrap(),
            RemoteClaim::Complete
        ));
        server.abort();
    }

    fn count_named_files(directory: &Path, name: &str) -> usize {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| {
                if path.is_dir() {
                    count_named_files(&path, name)
                } else {
                    usize::from(path.file_name().is_some_and(|file| file == name))
                }
            })
            .sum()
    }

    fn count_files_with_suffix(directory: &Path, suffix: &str) -> usize {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| {
                if path.is_dir() {
                    count_files_with_suffix(&path, suffix)
                } else {
                    usize::from(path.to_string_lossy().ends_with(suffix))
                }
            })
            .sum()
    }

    #[tokio::test]
    async fn reported_worker_exit_releases_the_row_and_is_idempotent() {
        let (_directory, client, selection, server) = fixture().await;
        let exited = client.clone().worker("crashed-worker");

        let RemoteClaim::Run {
            claim: stale,
            repetition,
            ..
        } = exited.claim(&selection).await.unwrap()
        else {
            panic!("first worker should run");
        };
        exited
            .worker_exited("worker process exited with signal 9")
            .await
            .unwrap();
        exited
            .worker_exited("duplicate process observation")
            .await
            .unwrap();
        let status = client.status().await.unwrap();
        assert_eq!(
            status["recent_attempts"]["failures"][0]["error"],
            "worker process exited with signal 9"
        );

        let replacement_client = client.clone().worker("replacement-worker");
        let RemoteClaim::Run {
            claim: replacement,
            repetition: replacement_repetition,
            ..
        } = replacement_client.claim(&selection).await.unwrap()
        else {
            panic!("the other pre-materialized row should be claimed");
        };
        assert_eq!(replacement_repetition, repetition);
        assert!(matches!(
            client.fail(&stale, "late worker").await,
            Err(CoordinatorError::Rejected {
                status: StatusCode::NOT_FOUND,
                ..
            })
        ));
        client.fail(&replacement, "test cleanup").await.unwrap();
        let RemoteClaim::Run { claim, .. } = replacement_client.claim(&selection).await.unwrap()
        else {
            panic!("the remaining pre-materialized row should be claimable");
        };
        client.fail(&claim, "test cleanup").await.unwrap();
        let status = client.status().await.unwrap();
        assert_eq!(status["tasks"]["failed"], 2);
        assert!(matches!(
            client.claim(&selection).await.unwrap(),
            RemoteClaim::Complete
        ));
        server.abort();
    }

    #[tokio::test]
    async fn infrastructure_failure_requeues_the_same_coordinate() {
        let (directory, client, selection, server) = fixture().await;
        let first = client.clone().worker("first-worker");
        let RemoteClaim::Run {
            claim, repetition, ..
        } = first.claim(&selection).await.unwrap()
        else {
            panic!("first worker should run");
        };
        first.retry(&claim, "provider returned 429").await.unwrap();

        let status = client.status().await.unwrap();
        assert_eq!(status["tasks"]["unclaimed"], 2);
        assert_eq!(status["tasks"]["failed"], 0);
        let replacement = client.clone().worker("replacement-worker");
        let RemoteClaim::Run {
            repetition: replacement_repetition,
            ..
        } = replacement.claim(&selection).await.unwrap()
        else {
            panic!("infrastructure-failed coordinate should be claimable");
        };
        assert_eq!(replacement_repetition, repetition);
        let connection = Connection::open(directory.path().join("state/state.sqlite3")).unwrap();
        let attempt: (String, String) = connection
            .query_row(
                "SELECT state, error FROM eval_attempts ORDER BY id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            attempt,
            (
                "infrastructure_failed".to_owned(),
                "provider returned 429".to_owned()
            )
        );
        server.abort();
    }

    #[tokio::test]
    async fn numeric_worker_ids_match_their_cli_string_identity() {
        let (_directory, client, selection, server) = fixture().await;
        let worker = client.clone().worker("7");
        assert!(matches!(
            worker.claim(&selection).await.unwrap(),
            RemoteClaim::Run { .. }
        ));

        accepted(
            client
                .http
                .post(client.endpoint("v1/workers/exited").unwrap())
                .json(&serde_json::json!({ "worker": 7, "error": "child exited" }))
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();

        let status = client.status().await.unwrap();
        assert_eq!(status["tasks"]["running"], 0);
        assert_eq!(status["tasks"]["failed"], 0);
        assert_eq!(status["tasks"]["unclaimed"], 2);
        server.abort();
    }

    #[tokio::test]
    async fn coordinator_restart_recovers_running_claim_from_sqlite() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path());
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"[profiles.release]
tasks = ["one"]
trials = 1
model = ["sol"]
thinking = ["high"]
"#,
        )
        .unwrap();
        let state = directory.path().join("state");
        Evaluation::add_profile(&config, Some("release"), &state, "release", false).unwrap();

        let workset = Workset::open(state.join("state.sqlite3"), "release").unwrap();
        let BeginTask::Run(claim) = workset.begin_next_for_worker("surviving-worker").unwrap()
        else {
            panic!("pre-materialized row should be claimable");
        };
        let remote_claim = RemoteTaskClaim {
            token: claim.id().to_owned(),
        };
        drop(claim);

        let evaluation = Evaluation::open(&config, Some("release"), &state).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            CoordinatorServer::new(evaluation)
                .serve(listener)
                .await
                .unwrap();
        });
        let client = CoordinatorClient::new(&format!("http://{address}")).unwrap();
        client
            .fail(&remote_claim, "worker finished after coordinator restart")
            .await
            .unwrap();

        let status = client.status().await.unwrap();
        assert_eq!(status["tasks"]["running"], 0);
        assert_eq!(status["tasks"]["failed"], 1);
        server.abort();
    }

    #[test]
    fn task_archives_extract_without_filesystem_authority() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path());
        let source = directory.path().join("one");
        let expected = Task::load(&source).unwrap().package_digest().to_owned();
        let archive_path = directory.path().join("task.tar.zst");
        let file = fs::File::create(&archive_path).unwrap();
        let encoder = zstd::Encoder::new(file, 3).unwrap();
        let mut archive = tar::Builder::new(encoder);
        archive
            .append_path_with_name(source.join("task.toml"), "task.toml")
            .unwrap();
        archive
            .append_path_with_name(source.join("instruction.md"), "instruction.md")
            .unwrap();
        archive
            .append_dir("environment", source.join("environment"))
            .unwrap();
        archive
            .append_path_with_name(
                source.join("environment/Dockerfile"),
                "environment/Dockerfile",
            )
            .unwrap();
        archive.append_dir("tests", source.join("tests")).unwrap();
        archive
            .append_path_with_name(source.join("tests/test.sh"), "tests/test.sh")
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();

        let staging = directory.path().join("staging");
        let output = directory.path().join("materialized");
        extract_task_package_archive(&archive_path, &staging, &output).unwrap();

        assert_eq!(Task::load(output).unwrap().package_digest(), expected);
    }

    #[test]
    fn mutation_requests_use_the_configured_bearer_credential() {
        let client = CoordinatorClient::new("http://127.0.0.1:8789")
            .unwrap()
            .write_token("eval-secret");
        let request = client
            .authorize(client.http.post(client.endpoint("v1/claims").unwrap()))
            .build()
            .unwrap();
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer eval-secret",
        );
    }

    #[test]
    fn plain_http_is_limited_to_loopback_addresses() {
        assert!(CoordinatorClient::new("http://192.0.2.1:8789").is_err());
        assert!(CoordinatorClient::new("http://100.64.0.1:8789").is_err());
        assert!(CoordinatorClient::new("http://100.127.255.255:8789").is_err());
        assert!(CoordinatorClient::new("http://127.0.0.1:8789").is_ok());
        assert!(CoordinatorClient::new("http://[::1]:8789").is_ok());
        assert!(CoordinatorClient::new("http://localhost:8789").is_ok());
        assert!(CoordinatorClient::new("https://evals.example").is_ok());
    }
}
