//! Application-owned tracing setup for Nanocodex spans.

use std::{fs::OpenOptions, io, path::PathBuf};

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource, runtime,
    trace::{
        SdkTracerProvider,
        span_processor_with_async_runtime::BatchSpanProcessor as TokioBatchSpanProcessor,
    },
};
use opentelemetry_semantic_conventions::{SCHEMA_URL, attribute::SERVICE_VERSION};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer, fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt,
};

const DEPLOYMENT_ENVIRONMENT_NAME: &str = "deployment.environment.name";

/// Human-readable or structured local tracing output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogFormat {
    Pretty,
    #[default]
    Compact,
    Json,
}

/// Destination for the local formatting layer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum LogOutput {
    #[default]
    Stderr,
    File(PathBuf),
}

/// Builder for local formatting and optional OTLP/HTTP trace export.
#[derive(Clone, Debug)]
pub struct ObservabilityBuilder {
    filter: String,
    otel_filter: String,
    format: LogFormat,
    output: LogOutput,
    service_name: String,
    service_version: String,
    environment: Option<String>,
    otlp_endpoint: Option<String>,
}

/// Keeps asynchronous formatting and OTLP providers alive and flushes them on drop.
pub struct ObservabilityGuard {
    tracer_provider: Option<SdkTracerProvider>,
    _writer: WorkerGuard,
}

#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    #[error("invalid tracing filter: {0}")]
    Filter(#[from] tracing_subscriber::filter::ParseError),
    #[error("failed to open tracing output: {0}")]
    Output(#[from] io::Error),
    #[error("failed to configure OTLP exporter: {0}")]
    Otlp(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("failed to flush or shut down the OpenTelemetry exporter: {0}")]
    OTelSdk(#[from] opentelemetry_sdk::error::OTelSdkError),
    #[error("a global tracing subscriber is already installed")]
    Subscriber,
}

impl ObservabilityBuilder {
    #[must_use]
    pub fn new(service_name: impl Into<String>, service_version: impl Into<String>) -> Self {
        let filter =
            "warn,nanocodex=info,nanocodex_service=info,nanocodex_tools=info,nanocodex_mcp=info"
                .to_owned();
        Self {
            otel_filter: filter.clone(),
            filter,
            format: LogFormat::Compact,
            output: LogOutput::Stderr,
            service_name: service_name.into(),
            service_version: service_version.into(),
            environment: None,
            otlp_endpoint: None,
        }
    }

    #[must_use]
    pub fn filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = filter.into();
        self
    }

    /// Sets the independent filter applied only to exported OpenTelemetry spans.
    #[must_use]
    pub fn otel_filter(mut self, filter: impl Into<String>) -> Self {
        self.otel_filter = filter.into();
        self
    }

    #[must_use]
    pub const fn format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    #[must_use]
    pub fn output(mut self, output: LogOutput) -> Self {
        self.output = output;
        self
    }

    #[must_use]
    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    /// Sets the OTLP/HTTP collector base endpoint.
    ///
    /// The standard `/v1/traces` signal path is appended unless it is already
    /// present, matching `OTEL_EXPORTER_OTLP_ENDPOINT` semantics.
    #[must_use]
    pub fn otlp_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        self.otlp_endpoint = Some(trace_endpoint(&endpoint));
        self
    }

    /// Installs the process-global subscriber.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid filter, unusable output file, invalid
    /// exporter configuration, or an already-installed global subscriber.
    pub fn install(self) -> Result<ObservabilityGuard, ObservabilityError> {
        let (writer, writer_guard) = tracing_appender::non_blocking(self.writer()?);
        let filter = EnvFilter::try_new(self.filter.as_str())?;
        let otel_filter = EnvFilter::try_new(self.otel_filter.as_str())?;
        let fmt_layer = match self.format {
            LogFormat::Pretty => tracing_subscriber::fmt::layer()
                .pretty()
                .with_writer(writer)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_filter(filter)
                .boxed(),
            LogFormat::Compact => tracing_subscriber::fmt::layer()
                .compact()
                .with_writer(writer)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_filter(filter)
                .boxed(),
            LogFormat::Json => tracing_subscriber::fmt::layer()
                .json()
                .with_span_list(true)
                .with_current_span(false)
                .with_writer(writer)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_filter(filter)
                .boxed(),
        };
        let tracer_provider = self.tracer_provider()?;
        let tracing_layer = tracer_provider.as_ref().map(|provider| {
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer(self.service_name))
                .with_filter(otel_filter)
        });
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(tracing_layer)
            .try_init()
            .map_err(|_| ObservabilityError::Subscriber)?;
        tracing::callsite::rebuild_interest_cache();
        Ok(ObservabilityGuard {
            tracer_provider,
            _writer: writer_guard,
        })
    }

    fn writer(&self) -> Result<Box<dyn io::Write + Send>, io::Error> {
        match &self.output {
            LogOutput::Stderr => Ok(Box::new(io::stderr())),
            LogOutput::File(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Ok(Box::new(
                    OpenOptions::new().create(true).append(true).open(path)?,
                ))
            }
        }
    }

    fn tracer_provider(&self) -> Result<Option<SdkTracerProvider>, ObservabilityError> {
        let Some(endpoint) = self.otlp_endpoint.as_deref() else {
            return Ok(None);
        };
        let resource = self.resource();
        if current_tokio_runtime_is_multi_thread() {
            let exporter = SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_http_client(reqwest::Client::new())
                .build()?;
            let processor = TokioBatchSpanProcessor::builder(exporter, runtime::Tokio).build();
            return Ok(Some(
                SdkTracerProvider::builder()
                    .with_resource(resource)
                    .with_span_processor(processor)
                    .build(),
            ));
        }

        // The async processor's synchronous flush waits for work scheduled on
        // Tokio. Keep the SDK's dedicated-thread processor when no runtime is
        // active or when a current-thread runtime could deadlock that flush.
        let exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .with_protocol(Protocol::HttpBinary)
            .with_http_client(reqwest::blocking::Client::new())
            .build()?;
        let processor = opentelemetry_sdk::trace::BatchSpanProcessor::builder(exporter).build();
        Ok(Some(
            SdkTracerProvider::builder()
                .with_resource(resource)
                .with_span_processor(processor)
                .build(),
        ))
    }

    fn resource(&self) -> Resource {
        Resource::builder()
            .with_service_name(self.service_name.clone())
            .with_schema_url(
                [
                    opentelemetry::KeyValue::new(SERVICE_VERSION, self.service_version.clone()),
                    opentelemetry::KeyValue::new(
                        DEPLOYMENT_ENVIRONMENT_NAME,
                        self.environment
                            .clone()
                            .unwrap_or_else(|| "development".to_owned()),
                    ),
                ],
                SCHEMA_URL,
            )
            .build()
    }
}

fn current_tokio_runtime_is_multi_thread() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

fn trace_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint.ends_with("/v1/traces") {
        endpoint.to_owned()
    } else {
        format!("{endpoint}/v1/traces")
    }
}

impl ObservabilityGuard {
    /// Flushes pending spans and shuts down the exporter. Later calls are no-ops.
    ///
    /// # Errors
    ///
    /// Returns an error when the exporter cannot flush or shut down cleanly.
    pub fn shutdown(&mut self) -> Result<(), ObservabilityError> {
        if let Some(provider) = self.tracer_provider.take() {
            provider.force_flush()?;
            provider.shutdown()?;
        }
        Ok(())
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        drop(self.shutdown());
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    const OTLP_TEST_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn resource_uses_configured_service_identity_and_semantic_schema() {
        let resource = ObservabilityBuilder::new("nanocodex-test", "1.2.3")
            .environment("test")
            .resource();

        assert_eq!(
            resource.get(&opentelemetry::Key::new("service.name")),
            Some(opentelemetry::Value::from("nanocodex-test"))
        );
        assert_eq!(
            resource.get(&opentelemetry::Key::new(SERVICE_VERSION)),
            Some(opentelemetry::Value::from("1.2.3"))
        );
        assert_eq!(
            resource.get(&opentelemetry::Key::new(DEPLOYMENT_ENVIRONMENT_NAME)),
            Some(opentelemetry::Value::from("test"))
        );
        assert_eq!(resource.schema_url(), Some(SCHEMA_URL));
    }

    #[test]
    fn async_export_requires_a_multithreaded_tokio_runtime() {
        assert!(!current_tokio_runtime_is_multi_thread());

        let current_thread = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(!current_thread.block_on(async { current_tokio_runtime_is_multi_thread() }));

        let multi_thread = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();
        assert!(multi_thread.block_on(async { current_tokio_runtime_is_multi_thread() }));
    }

    #[test]
    fn formatting_and_otlp_export_share_the_installed_span_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_seen, request_received) = mpsc::channel();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + OTLP_TEST_TIMEOUT;
            loop {
                assert!(Instant::now() < deadline, "OTLP exporter did not connect");
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::with_capacity(4 * 1024);
                        let mut chunk = [0_u8; 1024];
                        while request.len() < 16 * 1024 {
                            let read = stream.read(&mut chunk).unwrap_or_default();
                            if read == 0 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..read]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                        // The HTTP client may open and close an empty warm-up
                        // connection before it sends the export request.
                        if request.is_empty() {
                            continue;
                        }
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                            )
                            .unwrap();
                        request_seen.send(request).unwrap();
                        return;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("loopback OTLP listener failed: {error}"),
                }
            }
        });
        let log_path = std::env::temp_dir().join(format!(
            "nanocodex-observability-{}.jsonl",
            std::process::id()
        ));
        let mut guard = ObservabilityBuilder::new("nanocodex-test", "0.0.0")
            .filter("trace")
            .format(LogFormat::Json)
            .output(LogOutput::File(log_path.clone()))
            .otlp_endpoint(format!("http://{address}"))
            .install()
            .unwrap();
        {
            let span = tracing::info_span!("test.operation", work_units = 3);
            let _entered = span.enter();
            tracing::info!("test event");
        }
        guard.shutdown().unwrap();
        let request = request_received.recv_timeout(OTLP_TEST_TIMEOUT).unwrap();
        assert!(
            request.starts_with(b"POST ")
                && request
                    .windows(b"/v1/traces".len())
                    .any(|window| window == b"/v1/traces"),
            "unexpected OTLP request headers: {}",
            String::from_utf8_lossy(&request)
        );
        server.join().unwrap();
        drop(guard);
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.lines().any(|line| line.contains("test.operation")));
        std::fs::remove_file(log_path).unwrap();
    }
}
