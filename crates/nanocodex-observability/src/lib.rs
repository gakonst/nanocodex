//! Application-owned tracing setup for Nanocodex spans.

use std::{fs::OpenOptions, io, path::PathBuf};

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer, fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt,
};

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
    #[error("a global tracing subscriber is already installed")]
    Subscriber,
}

impl ObservabilityBuilder {
    #[must_use]
    pub fn new(service_name: impl Into<String>, service_version: impl Into<String>) -> Self {
        Self {
            filter:
                "warn,nanocodex=info,nanocodex_service=info,nanocodex_tools=info,nanocodex_mcp=info"
                    .to_owned(),
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
                .with_writer(writer)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_filter(filter)
                .boxed(),
        };
        let tracer_provider = self.tracer_provider()?;
        let tracing_layer = tracer_provider.as_ref().map(|provider| {
            tracing_opentelemetry::layer().with_tracer(provider.tracer(self.service_name))
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
        let exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .with_protocol(Protocol::HttpBinary)
            .build()?;
        let resource = Resource::builder()
            .with_service_name(self.service_name.clone())
            .with_attribute(opentelemetry::KeyValue::new(
                opentelemetry_semantic_conventions::attribute::SERVICE_VERSION,
                self.service_version.clone(),
            ))
            .with_attribute(opentelemetry::KeyValue::new(
                "deployment.environment.name",
                self.environment
                    .clone()
                    .unwrap_or_else(|| "development".to_owned()),
            ))
            .build();
        let processor = opentelemetry_sdk::trace::BatchSpanProcessor::builder(exporter).build();
        Ok(Some(
            SdkTracerProvider::builder()
                .with_resource(resource)
                .with_span_processor(processor)
                .build(),
        ))
    }
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
    pub fn shutdown(&mut self) {
        if let Some(provider) = self.tracer_provider.take() {
            drop(provider.force_flush());
            drop(provider.shutdown());
        }
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        self.shutdown();
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

    #[test]
    fn formatting_and_otlp_export_share_the_installed_span_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_seen, request_received) = mpsc::channel();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0_u8; 16 * 1024];
                        let read = stream.read(&mut request).unwrap_or_default();
                        let is_trace_request = request[..read]
                            .windows(b"POST /v1/traces".len())
                            .any(|window| window == b"POST /v1/traces");
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                            )
                            .unwrap();
                        request_seen.send(is_trace_request).unwrap();
                        return;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "OTLP exporter did not connect");
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
        guard.shutdown();
        assert!(
            request_received
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
        );
        server.join().unwrap();
        drop(guard);
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.lines().any(|line| line.contains("test.operation")));
        std::fs::remove_file(log_path).unwrap();
    }
}
