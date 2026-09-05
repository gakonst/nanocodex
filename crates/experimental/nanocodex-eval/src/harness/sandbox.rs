use super::*;

pub(super) struct HarnessVmResources {
    environment: VmEnvironment,
    backend: VmBackend,
}

pub(super) async fn prepare_harness_vm_resources(
    task: &Task,
    vm: &VmResources,
    guest_memory_mb: u64,
    web_search: bool,
    verifier_environment: &BTreeMap<String, String>,
) -> InternalResult<HarnessVmResources> {
    let environment = vm.environment(task).await?;
    let backend = vm
        .backend_for_task_with_guest_memory(
            VmBackend::builder()
                .retain_passed_rootfs(false)
                .retain_failed_rootfs(false)
                .force_agent_network(true)
                .web_search(web_search)
                .verifier_environment(verifier_environment.clone()),
            task,
            guest_memory_mb,
        )
        .await?;
    Ok(HarnessVmResources {
        environment,
        backend,
    })
}

impl HarnessVmResources {
    pub(super) fn backend(&self) -> VmBackend {
        self.backend.clone()
    }

    pub(super) fn harness_attempt(
        &self,
        mut runtime: VmAttempt,
        attempt: EvalAttempt<'_>,
        command: HarnessExec,
        guest_command: String,
        auth: HarnessAuth,
        guest: HarnessGuestConfig,
    ) -> InternalResult<AttemptAgent, VmAttemptError> {
        let capture_listener = runtime.take_capture_listener();
        let session = runtime.session_handle()?;
        let runner = VmHarnessRunner::new(
            session,
            attempt,
            &self.environment,
            guest_command,
            auth,
            guest,
            capture_listener,
        )?;
        let api_base_url = runner.api_base_url().to_owned();
        let runner = Arc::new(runner);
        let readiness = Arc::clone(&runner);
        Ok(runtime
            .harness(command.api_base_url(api_base_url).command_runner(runner))
            .ready(async move { readiness.prepare().await }))
    }
}

pub(super) enum GuestAuth {
    ApiKey(Arc<str>),
    AccessToken(Arc<str>),
    AuthFile(Vec<u8>),
}

pub(super) struct VmHarnessRunner {
    session: VmToolSessionHandle,
    guest_command: String,
    workspace: String,
    environment: Vec<(String, String)>,
    auth_file: Option<Vec<u8>>,
    harness_home: String,
    harness_auth_file: String,
    capture_upstream: String,
    capture_listener: Mutex<Option<TcpListener>>,
    capture_base_url: String,
    capture_only_port: Option<u16>,
    api_exchanges: PathBuf,
}

impl VmHarnessRunner {
    fn new(
        session: VmToolSessionHandle,
        attempt: EvalAttempt<'_>,
        environment: &VmEnvironment,
        guest_command: String,
        auth: HarnessAuth,
        guest: HarnessGuestConfig,
        capture_listener: Option<TcpListener>,
    ) -> InternalResult<Self, VmAttemptError> {
        if !Path::new(&guest.home).is_absolute() || !Path::new(&guest.auth_file).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "harness home and auth_file must be absolute guest paths",
            )
            .into());
        }
        if guest.api_key_environment.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "harness api_key_environment must not be empty",
            )
            .into());
        }
        let artifact_directory = attempt.directory().join("agent");
        fs::create_dir_all(&artifact_directory)?;
        let auth = match auth.kind {
            HarnessAuthKind::ApiKey(api_key) => GuestAuth::ApiKey(api_key),
            HarnessAuthKind::AccessToken(access_token) => GuestAuth::AccessToken(access_token),
            HarnessAuthKind::AuthFile(path) => {
                let contents = fs::read(&path)?;
                GuestAuth::AuthFile(contents)
            }
        };
        let capture_listener = match capture_listener {
            Some(listener) => listener,
            None if attempt.task().network() == crate::NetworkPolicy::Public => {
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            }
            None => {
                return Err(io::Error::other(
                    "capture-only harness VM did not reserve its capture listener",
                )
                .into());
            }
        };
        let capture_port = capture_listener.local_addr()?.port();
        let capture_base_url = capture_proxy_vm_base_url(capture_port);
        let capture_only_port =
            (attempt.task().network() == crate::NetworkPolicy::Disabled).then_some(capture_port);
        let mut command_environment = environment.guest_environment(attempt.task());
        command_environment.extend(guest.environment.into_iter().map(|(key, value)| {
            let value = value
                .replace("{api_base_url}", &capture_base_url)
                .replace("{harness_home}", &guest.home)
                .replace("{auth_file}", &guest.auth_file);
            (key, value)
        }));
        command_environment.insert("NANOCODEX_HARNESS_HOME".to_owned(), guest.home.clone());
        command_environment.insert(
            "NANOCODEX_HARNESS_AUTH_FILE".to_owned(),
            guest.auth_file.clone(),
        );
        command_environment.insert(
            "NANOCODEX_HARNESS_API_BASE_URL".to_owned(),
            capture_base_url.clone(),
        );
        let auth_file = match auth {
            GuestAuth::ApiKey(api_key) => {
                command_environment.insert(guest.api_key_environment.clone(), api_key.to_string());
                command_environment.remove("CODEX_ACCESS_TOKEN");
                None
            }
            GuestAuth::AccessToken(access_token) => {
                command_environment.remove(&guest.api_key_environment);
                command_environment
                    .insert("CODEX_ACCESS_TOKEN".to_owned(), access_token.to_string());
                None
            }
            GuestAuth::AuthFile(contents) => {
                command_environment.remove(&guest.api_key_environment);
                command_environment.remove("CODEX_ACCESS_TOKEN");
                Some(contents)
            }
        };
        let capture_upstream = guest
            .api_upstream
            .unwrap_or_else(|| HARNESS_CAPTURE_PROXY_API_UPSTREAM.to_owned());
        Ok(Self {
            session,
            guest_command,
            workspace: environment.workspace().to_owned(),
            environment: command_environment.into_iter().collect(),
            auth_file,
            harness_home: guest.home,
            harness_auth_file: guest.auth_file,
            capture_upstream,
            capture_listener: Mutex::new(Some(capture_listener)),
            capture_base_url,
            capture_only_port,
            api_exchanges: artifact_directory.join(HARNESS_API_EXCHANGES_FILENAME),
        })
    }

    fn api_base_url(&self) -> &str {
        &self.capture_base_url
    }

    async fn prepare(&self) -> InternalResult<(), VmAttemptError> {
        self.session.ready().await?;
        self.session
            .create_directory(&self.harness_home, 0o700, None)
            .await?;
        if let Some(auth_file) = &self.auth_file {
            let parent = Path::new(&self.harness_auth_file).parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "harness auth_file has no parent",
                )
            })?;
            self.session
                .create_directory(parent.to_string_lossy(), 0o700, None)
                .await?;
            self.session
                .write_file(&self.harness_auth_file, auth_file.clone(), 0o600)
                .await?;
        }
        Ok(())
    }

    async fn start_capture_proxy(
        &self,
    ) -> InternalResult<ResponsesCaptureProxy, HarnessCommandRunnerError> {
        let listener = {
            let mut listener = self.capture_listener.lock().map_err(|_| {
                HarnessCommandRunnerError::new("Responses capture listener lock was poisoned")
            })?;
            listener.take().ok_or_else(|| {
                HarnessCommandRunnerError::new(
                    "Responses capture proxy was already started for this attempt",
                )
            })?
        };
        let proxy = ResponsesCaptureProxy::start(
            listener,
            ResponsesCaptureProxyConfig {
                upstream: self.capture_upstream.to_owned(),
                output: self.api_exchanges.clone(),
            },
        )
        .await
        .map_err(|error| HarnessCommandRunnerError::new(error.to_string()))?;
        Ok(proxy)
    }

    async fn stop_capture_proxy(
        &self,
        proxy: ResponsesCaptureProxy,
    ) -> InternalResult<(), HarnessCommandRunnerError> {
        match tokio::time::timeout(HARNESS_CAPTURE_PROXY_STOP_TIMEOUT, proxy.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(HarnessCommandRunnerError::new(error.to_string())),
            Err(_) => {
                return Err(HarnessCommandRunnerError::new(format!(
                    "Responses capture proxy did not stop within {:?}",
                    HARNESS_CAPTURE_PROXY_STOP_TIMEOUT
                )));
            }
        }
        Ok(())
    }
}

pub(super) fn capture_proxy_vm_base_url(port: u16) -> String {
    format!("http://{}:{port}", Gvproxy::HOST_IPV4)
}

impl HarnessCommandRunner for VmHarnessRunner {
    fn run<'a>(
        &'a self,
        arguments: Vec<String>,
        timeout: Duration,
    ) -> Pin<
        Box<
            dyn Future<Output = InternalResult<HarnessCommandOutput, HarnessCommandRunnerError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let capture_proxy = self.start_capture_proxy().await?;
            let mut command = if let Some(port) = self.capture_only_port {
                VmCommand::new(CAPTURE_ONLY_GUEST_RUNTIME)
                    .arg("--capture-only")
                    .arg(port.to_string())
                    .arg(&self.guest_command)
            } else {
                VmCommand::new(&self.guest_command)
            }
            .current_directory(&self.workspace)
            .environment(self.environment.clone())
            .timeout(timeout)
            .max_output_bytes(HARNESS_OUTPUT_BYTES);
            for argument in arguments {
                command = command.arg(argument);
            }
            let result = self.session.command(command).await;
            self.stop_capture_proxy(capture_proxy).await?;
            match result {
                Ok(output) => Ok(HarnessCommandOutput {
                    status: HarnessCommandStatus::Exited(output.exit_code),
                    stdout: output.stdout,
                    stderr: output.stderr,
                }),
                Err(VmToolSessionError::GuestTimeout { output, .. }) => Ok(HarnessCommandOutput {
                    status: HarnessCommandStatus::TimedOut,
                    stdout: output.stdout,
                    stderr: output.stderr,
                }),
                Err(error) => Err(HarnessCommandRunnerError::new(error.to_string())),
            }
        })
    }
}

pub(super) fn validate_vm_guest_elf(bytes: &[u8], path: &Path) -> InternalResult<()> {
    let header = bytes.get(..20).ok_or_else(|| {
        harness_error!(
            "VM guest executable is too short to contain an ELF header: {}",
            path.display()
        )
    })?;
    if &header[..4] != b"\x7fELF" {
        return Err(harness_error!(
            "VM guest executable is not an ELF executable: {}",
            path.display()
        ));
    }
    let class = header[4];
    let byte_order = header[5];
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if class != 2 || byte_order != 1 || machine != VM_GUEST_ELF_MACHINE {
        return Err(harness_error!(
            "VM guest executable {} has ELF class {class}, byte order {byte_order}, and e_machine \
             {machine}; target {VM_GUEST_TARGET} requires 64-bit little-endian e_machine \
             {VM_GUEST_ELF_MACHINE}",
            path.display()
        ));
    }
    Ok(())
}
