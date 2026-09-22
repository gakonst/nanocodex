//! Agent-only semantic observations. Local configuration never comes from tool payloads.
//! Each provider is a cancellable child process, with bounded output and a shared
//! concurrency gate. Dropping a collection kills its children; no blocking tasks
//! or detached provider threads survive an agent cancellation.
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

const HELPER: &str = include_str!("observation_providers/helper.py");
const DEADLINE: Duration = Duration::from_millis(600);
const OUTPUT_LIMIT: u64 = 12_288;
const DATA_LIMIT: usize = 8192;
static CHILDREN: Semaphore = Semaphore::const_new(8);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Context {
    app: String,
    window: String,
}
impl Context {
    pub(crate) fn parse(value: Option<&Value>) -> Result<Option<Self>, ()> {
        let Some(value) = value else { return Ok(None) };
        let context: Self = serde_json::from_value(value.clone()).map_err(|_| ())?;
        if [&context.app, &context.window]
            .iter()
            .any(|s| s.is_empty() || s.len() > 512 || s.chars().any(char::is_control))
        {
            return Err(());
        }
        Ok(Some(context))
    }
}
#[derive(Clone)]
enum Provider {
    Atspi {
        bus: Option<String>,
    },
    External(PathBuf),
    Unavailable,
    #[cfg(test)]
    Stalled,
}
#[derive(Clone)]
pub(crate) struct Registry(Arc<Vec<(String, Provider)>>);
impl Registry {
    /// Only NativeScreen may opt into local provider configuration. A VM screen
    /// must not read its publisher's host files or host accessibility bus.
    pub(crate) fn local() -> Self {
        let mut providers = vec![(
            "atspi".into(),
            Provider::Atspi {
                // Native Linux capture is a private Xvfb desktop. Never borrow the
                // publisher's unrelated ambient user session bus.
                bus: std::env::var("NANOCODEX_OBSERVATION_ATSPI_BUS")
                    .ok()
                    .filter(|s| !s.is_empty() && s.len() <= 4096),
            },
        )];
        if let Ok(config) = std::env::var("NANOCODEX_OBSERVATION_SNAPSHOT_PATHS")
            && config.len() <= 16_384
            && let Ok(paths) = serde_json::from_str::<Vec<String>>(&config)
        {
            for (index, path) in paths.into_iter().take(4).enumerate() {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    providers.push((format!("external:{index}"), Provider::External(path)));
                }
            }
        }
        Self(Arc::new(providers))
    }
    #[cfg(test)]
    pub(crate) fn stalled() -> Self {
        Self(Arc::new(vec![("stalled".into(), Provider::Stalled)]))
    }
    pub(crate) fn remote() -> Self {
        Self(Arc::new(vec![(
            "desktop-context".into(),
            Provider::Unavailable,
        )]))
    }
    /// `captured_at` is the collection-start anchor, not a hardware frame timestamp.
    /// Provider timestamps refer to their source snapshot (or completion when no
    /// source timestamp is available); this envelope does not assert synchronization.
    pub(crate) async fn collect(
        &self,
        context: Option<Context>,
        captured_at: u64,
        budget: Duration,
    ) -> Value {
        let providers = join_all(self.0.iter().map(|(id, provider)| async {
            let result =
                match tokio::time::timeout(budget.min(DEADLINE), provider.run(context.as_ref()))
                    .await
                {
                    Ok(result) => result,
                    Err(_) => outcome("timeout", "deadline_exceeded"),
                };
            let mut result = normalize(id, result);
            let successful = matches!(result["status"].as_str(), Some("ok" | "partial"));
            result["scope"] = json!(if context.is_some() {
                "requested_context"
            } else if successful {
                "active_window"
            } else {
                "none"
            });
            result["foreground_verified"] = json!(
                successful && context.is_none() && matches!(provider, Provider::Atspi { .. })
            );
            result
        }))
        .await;
        json!({"schemaVersion":1,"capturedAt":captured_at,"providers":providers})
    }
}
fn outcome(status: &str, error: &str) -> Value {
    json!({"status":status,"error":error})
}
impl Provider {
    async fn run(&self, context: Option<&Context>) -> Value {
        match self {
            #[cfg(test)]
            Self::Stalled => return std::future::pending().await,
            Self::Unavailable => return outcome("unavailable", "remote_providers_unavailable"),
            Self::Atspi { .. } if !cfg!(target_os = "linux") => {
                return outcome("unavailable", "unsupported_platform");
            }
            Self::Atspi { bus: None } => return outcome("unavailable", "session_bus_unavailable"),
            Self::External(_) if context.is_none() => {
                return outcome("unavailable", "context_required");
            }
            _ => {}
        }
        if !cfg!(unix) {
            return outcome("unavailable", "unsupported_platform");
        }
        let Ok(_permit) = CHILDREN.try_acquire() else {
            return outcome("unavailable", "provider_busy");
        };
        let mut command = Command::new("python3");
        command.args(["-I", "-X", "utf8", "-c", HELPER]);
        command.env_remove("DBUS_SESSION_BUS_ADDRESS");
        command.env_remove("AT_SPI_BUS_ADDRESS");
        match self {
            Self::Atspi { bus } => {
                command.arg("atspi");
                if let Some(bus) = bus {
                    command.env("DBUS_SESSION_BUS_ADDRESS", bus);
                }
            }
            Self::External(path) => {
                command.arg("external").arg(path);
            }
            Self::Unavailable => unreachable!(),
            #[cfg(test)]
            Self::Stalled => unreachable!(),
        }
        run_child(command, context).await
    }
}
async fn run_child(mut command: Command, context: Option<&Context>) -> Value {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return outcome("unavailable", "python_unavailable");
    };
    let result: Result<Value, ()> = async {
        let mut stdin = child.stdin.take().ok_or(())?;
        stdin
            .write_all(json!({"context":context}).to_string().as_bytes())
            .await
            .map_err(|_| ())?;
        drop(stdin);
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or(())?
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ())?;
        if bytes.len() > OUTPUT_LIMIT as usize {
            return Err(());
        }
        if !child.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        serde_json::from_slice(&bytes).map_err(|_| ())
    }
    .await;
    result.unwrap_or_else(|_| outcome("error", "invalid_provider_output"))
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn normalize(id: &str, raw: Value) -> Value {
    let now = now_ms();
    let status = raw["status"]
        .as_str()
        .filter(|s| ["ok", "partial", "unavailable", "error", "timeout"].contains(s))
        .unwrap_or("error");
    let timestamp = raw["capturedAt"]
        .as_u64()
        .filter(|t| *t > 0 && *t <= now.saturating_add(1000));
    let mut result = json!({"id":id,"status":status,"capturedAt":timestamp.unwrap_or(now),"freshness":"unknown"});
    if let Some(timestamp) = timestamp {
        let age = now.saturating_sub(timestamp);
        result["ageMs"] = json!(age);
        result["freshness"] = json!(if age > 5000 { "stale" } else { "fresh" });
    }
    if let Some(error) = raw["error"]
        .as_str()
        .filter(|s| s.len() <= 80 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    {
        result["error"] = json!(error);
    }
    if ["ok", "partial"].contains(&status) {
        if timestamp.is_some()
            && raw["data"].is_object()
            && raw["data"].to_string().len() <= DATA_LIMIT
        {
            result["data"] = raw["data"].clone();
        } else {
            result["status"] = json!("error");
            result["error"] = json!("invalid_provider_output");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_and_reaps_helper() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut command = Command::new("python3");
        command
            .args([
                "-I",
                "-c",
                "import os,sys,time; open(sys.argv[1],'w').write(str(os.getpid())); time.sleep(30)",
            ])
            .arg(file.path());
        let mut running = Box::pin(run_child(command, None));
        let pid: i32 = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    result = &mut running => panic!("helper exited before cancellation: {result}"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {
                        if let Ok(pid) = std::fs::read_to_string(file.path()).unwrap().parse() { break pid; }
                    }
                }
            }
        }).await.expect("helper must start");
        drop(running);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled helper must be killed and reaped");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn helper_output_is_bounded_without_waiting_for_exit() {
        let mut command = Command::new("python3");
        command.args([
            "-I",
            "-c",
            "import sys,time; sys.stdout.write('x'*20000); sys.stdout.flush(); time.sleep(30)",
        ]);
        let result = tokio::time::timeout(Duration::from_secs(2), run_child(command, None))
            .await
            .unwrap();
        assert_eq!(result["error"], "invalid_provider_output");
    }
    #[test]
    fn context_requires_exact_bounded_identity() {
        assert!(Context::parse(None).unwrap().is_none());
        for invalid in [
            json!({"app":"x"}),
            json!({"app":"x","window":"y","path":"/secret"}),
            json!({"app":"","window":"y"}),
            json!({"app":"x","window":"a\nb"}),
        ] {
            assert!(Context::parse(Some(&invalid)).is_err());
        }
        assert!(Context::parse(Some(&json!({"app":"app","window":"title"}))).is_ok());
    }
    #[test]
    fn timestamps_and_payloads_are_checked() {
        let stale = normalize(
            "test",
            json!({"status":"ok","capturedAt":now_ms()-6000,"data":{"x":1}}),
        );
        assert_eq!(stale["freshness"], "stale");
        assert_eq!(
            normalize("test", json!({"status":"ok","data":{}}))["status"],
            "error"
        );
        assert_eq!(
            normalize(
                "test",
                json!({"status":"ok","capturedAt":now_ms(),"data":{"x":"a".repeat(DATA_LIMIT)}})
            )["status"],
            "error"
        );
        assert!(
            normalize("test", json!({"status":"error","error":"/secret/path"}))
                .get("error")
                .is_none()
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn real_external_helper_is_scoped_and_bounded() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), json!({"schemaVersion":1,"capturedAt":now_ms(),"app":"example","window":"main","data":{"label":"hello"}}).to_string()).unwrap();
        let registry = Registry(Arc::new(vec![(
            "external:0".into(),
            Provider::External(file.path().into()),
        )]));
        let context = Context::parse(Some(&json!({"app":"example","window":"main"}))).unwrap();
        let result = registry.collect(context, now_ms(), DEADLINE).await;
        assert_eq!(result["providers"][0]["status"], "ok", "{result}");
        assert_eq!(result["providers"][0]["data"]["label"], "hello");
        assert_eq!(result["providers"][0]["scope"], "requested_context");
        assert_eq!(result["providers"][0]["foreground_verified"], false);
        // UTF-8 payloads fit the data/output caps without ASCII escape expansion.
        std::fs::write(file.path(), json!({"schemaVersion":1,"capturedAt":now_ms(),"app":"example","window":"main","data":{"labels":vec!["語".repeat(170);15]}}).to_string()).unwrap();
        let context = Context::parse(Some(&json!({"app":"example","window":"main"}))).unwrap();
        let result = registry.collect(context, now_ms(), DEADLINE).await;
        assert_eq!(result["providers"][0]["status"], "ok", "{result}");
        let context = Context::parse(Some(&json!({"app":"other","window":"main"}))).unwrap();
        let result = registry.collect(context, now_ms(), DEADLINE).await;
        assert_eq!(result["providers"][0]["error"], "context_mismatch");
        assert!(result["providers"][0].get("data").is_none());
    }
    #[tokio::test]
    async fn remote_scope_never_reads_host_configuration() {
        let result = Registry::remote().collect(None, 42, DEADLINE).await;
        assert_eq!(result["capturedAt"], 42);
        assert_eq!(
            result["providers"][0]["error"],
            "remote_providers_unavailable"
        );
    }
    #[tokio::test]
    async fn external_without_context_does_not_open_file() {
        assert_eq!(
            Provider::External("/nonexistent".into()).run(None).await["error"],
            "context_required"
        );
    }
}
