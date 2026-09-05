use std::{
    process::Command as StdCommand,
    sync::atomic::{AtomicUsize, Ordering},
};

use nanocodex_agent::{Nanocodex, OpenAi};
use nanocodex_vm::tools::{VmCommandOutput, VmCommandPartialOutput};
use nix::unistd::getpgrp;

use super::*;

// VM lifecycle regression tests are kept beside the backend that owns the
// behavior rather than in the CLI adapter.
#[test]
fn evaluator_vm_builder_records_the_backend_environment() {
    let output = tempfile::tempdir().unwrap();
    let backend = VmBackend::builder().build();
    let openai = OpenAi::new("test").unwrap();
    let evaluator = crate::Evaluator::builder(Nanocodex::builder(openai), backend)
        .output_directory(output.path())
        .build()
        .unwrap();

    assert_eq!(evaluator.attempt_environment(), EvalEnvironment::MicroVm);
}

#[test]
fn run_scoped_judge_credentials_are_verifier_only() {
    let task =
        Task::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks/write-greeting"))
            .unwrap();
    let runtime = BTreeMap::from([("NANOCODEX_JUDGE_TOKEN".to_owned(), "run-secret".to_owned())]);

    let candidate = base_guest_environment(&task, "/workspace")
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let verifier = verifier_guest_environment(&task, "/workspace", &runtime)
        .into_iter()
        .collect::<BTreeMap<_, _>>();

    assert!(!candidate.contains_key("NANOCODEX_JUDGE_TOKEN"));
    assert_eq!(verifier["NANOCODEX_JUDGE_TOKEN"], "run-secret");
}

#[test]
fn eval_guest_memory_cap_only_reduces_large_task_allocations() {
    assert_eq!(effective_guest_memory_mb(8_192, None), 8_192);
    assert_eq!(effective_guest_memory_mb(8_192, Some(1_024)), 1_024);
    assert_eq!(effective_guest_memory_mb(256, Some(1_024)), 256);
    assert_eq!(effective_guest_memory_mb(0, Some(1_024)), 1);
    assert_eq!(
        effective_guest_memory_mb(u64::MAX, None),
        u64::from(u32::MAX)
    );
}

#[test]
fn guest_executables_are_installed_by_the_task_image_recipe() {
    let context = tempfile::tempdir().unwrap();
    fs::write(context.path().join("Dockerfile"), "FROM ubuntu:24.04\n").unwrap();
    let binary = context.path().join("codex");
    fs::write(&binary, b"codex-binary").unwrap();

    let installed = materialize_guest_executables(
        context.path(),
        &[VmGuestExecutable {
            source: binary,
            guest_path: "/usr/local/bin/codex".to_owned(),
        }],
    )
    .unwrap();

    assert_eq!(installed, 12);
    assert_eq!(
        fs::read_to_string(context.path().join("Dockerfile")).unwrap(),
        "FROM ubuntu:24.04\n\nCOPY .nanocodex/guest-executables/0 /usr/local/bin/codex\n"
    );
    let staged = context.path().join(".nanocodex/guest-executables/0");
    assert_eq!(fs::read(staged).unwrap(), b"codex-binary");
}

#[test]
fn artifact_archive_options_precede_the_path_terminator() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("tests")).unwrap();
    fs::create_dir(directory.path().join("environment")).unwrap();
    fs::write(
        directory.path().join("task.toml"),
        r#"
schema_version = "1.1"
artifacts = [
  { source = "/workspace", exclude = ["Dockerfile", "instruction.md"] },
  "/--option-shaped-output",
]

[task]
name = "adapter/artifact-order"

[agent]
timeout_sec = 1.0

[verifier]
timeout_sec = 1.0

[environment]
docker_image = "example/task:latest"
cpus = 1
memory_mb = 1
storage_mb = 1
"#,
    )
    .unwrap();
    fs::write(directory.path().join("instruction.md"), "Create output.").unwrap();
    fs::write(directory.path().join("tests/test.sh"), "exit 0\n").unwrap();
    fs::write(
        directory.path().join("environment/Dockerfile"),
        "FROM scratch\n",
    )
    .unwrap();
    let task = Task::load(directory.path()).unwrap();

    let arguments = artifact_archive_arguments(&task).unwrap();

    assert_eq!(
        arguments,
        [
            "-C",
            "/",
            "-cf",
            "/tmp/nanoeval-artifacts.tar",
            "--exclude=workspace/Dockerfile",
            "--exclude=workspace/instruction.md",
            "--",
            "workspace",
            "--option-shaped-output",
        ]
    );
}

#[tokio::test]
async fn vm_resources_leave_task_environments_lazy_and_single_flight() {
    let task =
        Task::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks/write-greeting"))
            .unwrap();
    let assets = tempfile::tempdir().unwrap();
    let rootfs = assets.path().join("rootfs");
    let gvproxy = assets.path().join("gvproxy");
    fs::create_dir(&rootfs).unwrap();
    fs::write(&gvproxy, []).unwrap();
    let resources = VmResources::builder(
        assets.path().join("vmm"),
        assets.path().join("runtime.ext4"),
    )
    .task(task.clone())
    .rootfs(&rootfs)
    .gvproxy(gvproxy)
    .image_preparation_concurrency(1)
    .prepare()
    .await
    .unwrap();
    let cell = resources.environments.get(task.root()).unwrap();
    assert!(cell.get().is_none());

    let (first, second) = tokio::join!(resources.environment(&task), resources.environment(&task));
    assert_eq!(first.unwrap(), second.unwrap());
    assert!(cell.get().is_some());
}

#[tokio::test]
async fn image_preparation_retries_only_recognized_network_failures() {
    let network_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::clone(&network_attempts);
    let prepared = prepare_image_with_network_retries(
        "task",
        "task",
        2,
        move || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(ImageError::BuildStep {
                        stage: 0,
                        instruction: 1,
                        exit_code: 6,
                        stdout: String::new(),
                        stderr: "curl: (6) Could not resolve host: example.com".to_owned(),
                    })
                } else {
                    Ok(7_u8)
                }
            }
        },
        |_| std::future::ready(()),
    )
    .await
    .unwrap();
    assert_eq!(prepared, 7);
    assert_eq!(network_attempts.load(Ordering::SeqCst), 2);

    let deterministic_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::clone(&deterministic_attempts);
    let error = prepare_image_with_network_retries(
        "task",
        "task",
        2,
        move || {
            attempts.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err::<u8, _>(ImageError::BuildStep {
                stage: 0,
                instruction: 1,
                exit_code: 1,
                stdout: String::new(),
                stderr: "compiler rejected invalid source".to_owned(),
            }))
        },
        |_| std::future::ready(()),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ImageError::BuildStep { .. }));
    assert_eq!(deterministic_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        (0..=2).map(image_network_retry_delay).collect::<Vec<_>>(),
        [2, 4, 8].map(Duration::from_secs)
    );
}

#[test]
fn backend_configuration_is_single_assignment() {
    let backend = VmBackend::builder().build();
    backend
        .configure(VmBackendConfiguration::builder("vmm", "runtime").build())
        .unwrap();

    let error = backend
        .configure(VmBackendConfiguration::builder("other-vmm", "other-runtime").build())
        .unwrap_err();

    assert!(matches!(error, VmBackendConfigureError::AlreadyConfigured));
}

#[tokio::test]
async fn evaluator_vmm_inherits_the_worker_process_group() {
    let inherited = recorded_vm_process_group().await;
    let parent_group = getpgrp().as_raw();

    assert_eq!(inherited.1, parent_group);
    assert_ne!(inherited.0, inherited.1);
}

async fn recorded_vm_process_group() -> (i32, i32) {
    let directory = tempfile::tempdir().unwrap();
    let vmm = directory.path().join("fake-vmm");
    let record = directory.path().join("process-group");
    fs::write(
        &vmm,
        "#!/bin/sh\n\
             directory=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
             pid=$$\n\
             pgid=$(ps -o pgid= -p \"$pid\" | tr -d ' ')\n\
             printf '%s %s\\n' \"$pid\" \"$pgid\" > \"$directory/process-group\"\n\
             exec /bin/sleep 30\n",
    )
    .unwrap();
    fs::set_permissions(&vmm, fs::Permissions::from_mode(0o700)).unwrap();
    let launch = VmLaunch {
        root: VmLaunchRoot::Directory(directory.path().join("root")),
        workspace: "/workspace".to_owned(),
        shell: "/bin/sh".to_owned(),
        runtime_image: directory.path().join("runtime"),
        vmm,
        cpus: 1,
        memory_mib: 128,
        resolver_configuration: String::new(),
        environment: BTreeMap::new(),
        network_socket: None,
        shared_directories: Vec::new(),
    };

    let session = launch.spawn(None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let values = loop {
        if let Ok(contents) = fs::read_to_string(&record)
            && let Ok(values) = contents
                .split_whitespace()
                .map(str::parse::<i32>)
                .collect::<Result<Vec<_>, _>>()
            && values.len() == 2
        {
            break values;
        }
        assert!(
            Instant::now() < deadline,
            "{} did not contain a complete process-group record",
            record.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    drop(session);
    (values[0], values[1])
}

#[test]
fn vm_bootstrap_preserves_shell_words_without_libkrun_quotes() {
    let workspace = "/workspace with 'single' and \"double\"";
    let quoted = super::shell_word_without_double_quotes(workspace);
    let script = format!("printf %s {quoted}");
    assert!(!script.contains('"'));
    let output = StdCommand::new("/bin/sh")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, workspace.as_bytes());

    let bootstrap = super::vm_guest_bootstrap_script(workspace, "nameserver 192.168.127.1\\n");
    assert!(!bootstrap.contains('"'));
    assert!(bootstrap.contains(&quoted));
}

#[test]
fn guest_agents_discovery_stays_inside_the_selected_guest_project() {
    let directory = tempfile::tempdir().unwrap();
    let unrelated_parent = directory.path().join("host-parent");
    let workspace = unrelated_parent.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(unrelated_parent.join("AGENTS.md"), "must not leak").unwrap();

    let output = StdCommand::new("/bin/sh")
        .args([
            "-c",
            GUEST_PROJECT_INSTRUCTION_PATHS_SCRIPT,
            "nanocodex-agents-md",
            workspace.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn guest_agents_discovery_prefers_overrides_and_returns_cwd_to_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    let nested = root.join("nested");
    let workspace = nested.join("workspace");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(root.join("AGENTS.md"), "root").unwrap();
    fs::write(nested.join("AGENTS.md"), "shadowed").unwrap();
    fs::write(nested.join("AGENTS.override.md"), "override").unwrap();

    let output = StdCommand::new("/bin/sh")
        .args([
            "-c",
            GUEST_PROJECT_INSTRUCTION_PATHS_SCRIPT,
            "nanocodex-agents-md",
            workspace.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(std::str::from_utf8(path).unwrap()))
        .collect::<Vec<_>>();

    assert!(output.status.success());
    assert_eq!(
        paths,
        [nested.join("AGENTS.override.md"), root.join("AGENTS.md")]
    );
}

#[test]
fn rootfs_cleanup_removes_only_a_disk_file() {
    let directory = tempfile::tempdir().unwrap();
    let rootfs = directory.path().join("rootfs.ext4");
    fs::write(&rootfs, b"guest disk").unwrap();

    assert!(remove_rootfs(&rootfs).unwrap());
    assert!(!rootfs.exists());
    assert!(!remove_rootfs(directory.path()).unwrap());
}

#[test]
fn disposable_attempt_root_is_a_sparse_overlay_over_the_immutable_template() {
    let directory = tempfile::tempdir().unwrap();
    let template = directory.path().join("template.ext4");
    let runtime = directory.path().join("runtime.ext4");
    let attempt = directory.path().join("attempt");
    fs::File::create(&template)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    fs::write(&runtime, b"runtime").unwrap();
    fs::create_dir(&attempt).unwrap();

    let root = materialize_attempt_root(
        &template,
        &runtime,
        &attempt,
        "rootfs",
        AttemptRootPolicy::DisposableOverlay,
    )
    .unwrap();

    let VmLaunchRoot::OverlayExt4 { lower, upper } = root else {
        panic!("disposable block root did not use guest OverlayFS");
    };
    assert_eq!(lower, template);
    assert_eq!(upper, attempt.join("rootfs.upper.ext4"));
    let mut disk = Reader::new(&upper).unwrap();
    assert!(disk.exists("/upper"));
    assert!(disk.exists("/work"));
}

#[test]
fn overlay_cleanup_removes_only_the_writable_delta() {
    let directory = tempfile::tempdir().unwrap();
    let lower = directory.path().join("base.ext4");
    let upper = directory.path().join("rootfs.upper.ext4");
    fs::write(&lower, b"immutable base").unwrap();
    fs::write(&upper, b"attempt delta").unwrap();
    let mut verifier = verifier_with_launch_root(
        VmLaunchRoot::OverlayExt4 {
            lower: lower.clone(),
            upper: upper.clone(),
        },
        false,
    );

    verifier.remove_disposable_root_disks(false).unwrap();

    assert!(lower.exists());
    assert!(!upper.exists());
}

#[test]
fn dropping_a_cancelled_overlay_attempt_removes_its_delta() {
    let directory = tempfile::tempdir().unwrap();
    let lower = directory.path().join("base.ext4");
    let upper = directory.path().join("rootfs.upper.ext4");
    fs::write(&lower, b"immutable base").unwrap();
    fs::write(&upper, b"cancelled attempt delta").unwrap();

    drop(verifier_with_launch_root(
        VmLaunchRoot::OverlayExt4 {
            lower: lower.clone(),
            upper: upper.clone(),
        },
        false,
    ));

    assert!(lower.exists());
    assert!(!upper.exists());
}

#[test]
fn backend_retains_only_failed_rootfs_by_default() {
    let backend = VmBackend::builder().build();
    assert!(!backend.retain_passed_rootfs);
    assert!(backend.retain_failed_rootfs);

    let trace_only = VmBackend::builder().retain_failed_rootfs(false).build();
    assert!(!trace_only.retain_passed_rootfs);
    assert!(!trace_only.retain_failed_rootfs);
}

#[test]
fn trace_only_cleanup_removes_a_failed_attempt_rootfs() {
    let directory = tempfile::tempdir().unwrap();
    let rootfs = directory.path().join("rootfs.ext4");
    fs::write(&rootfs, b"failed guest disk").unwrap();
    let mut verifier = verifier_with_rootfs(rootfs.clone(), false);

    verifier.remove_disposable_root_disks(false).unwrap();
    assert!(!rootfs.exists());
}

#[test]
fn trace_only_drop_removes_a_cancelled_attempt_rootfs() {
    let directory = tempfile::tempdir().unwrap();
    let rootfs = directory.path().join("rootfs.ext4");
    fs::write(&rootfs, b"cancelled guest disk").unwrap();

    drop(verifier_with_rootfs(rootfs.clone(), false));

    assert!(!rootfs.exists());
}

#[test]
fn failed_rootfs_retention_survives_verifier_drop() {
    let directory = tempfile::tempdir().unwrap();
    let rootfs = directory.path().join("rootfs.ext4");
    fs::write(&rootfs, b"retained guest disk").unwrap();

    drop(verifier_with_rootfs(rootfs.clone(), true));

    assert!(rootfs.exists());
}

#[test]
fn passed_rootfs_retention_survives_verifier_drop() {
    let directory = tempfile::tempdir().unwrap();
    let rootfs = directory.path().join("rootfs.ext4");
    fs::write(&rootfs, b"retained passing guest disk").unwrap();
    let mut verifier = verifier_with_rootfs(rootfs.clone(), false);
    verifier.retain_passed_rootfs = true;

    verifier.remove_disposable_root_disks(true).unwrap();
    drop(verifier);

    assert!(rootfs.exists());
}

#[test]
fn setup_guard_removes_unowned_disks_and_attempt_cache() {
    let directory = tempfile::tempdir().unwrap();
    let rootfs = directory.path().join("rootfs.ext4");
    let verifier_rootfs = directory.path().join("verifier-rootfs.ext4");
    let attempt_cache = directory.path().join("cache.ext4");
    for path in [&rootfs, &verifier_rootfs, &attempt_cache] {
        fs::write(path, b"disposable disk").unwrap();
    }
    let mut guard = VmAttemptSetupGuard::new(false);
    guard.track_root_disk(rootfs.clone());
    guard.track_root_disk(verifier_rootfs.clone());
    guard.track_attempt_cache(attempt_cache.clone());

    drop(guard);

    assert!(!rootfs.exists());
    assert!(!verifier_rootfs.exists());
    assert!(!attempt_cache.exists());
}

fn verifier_with_rootfs(rootfs: PathBuf, retain_failed_rootfs: bool) -> VmVerifier {
    verifier_with_launch_root(VmLaunchRoot::Ext4(rootfs), retain_failed_rootfs)
}

fn verifier_with_launch_root(root: VmLaunchRoot, retain_failed_rootfs: bool) -> VmVerifier {
    let directory = root
        .writable_disk()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    VmVerifier {
        agent_session: None,
        launch: VmLaunch {
            root,
            workspace: "/workspace".to_owned(),
            shell: "/bin/sh".to_owned(),
            runtime_image: directory.join("runtime"),
            vmm: directory.join("vmm"),
            cpus: 1,
            memory_mib: 256,
            resolver_configuration: String::new(),
            environment: BTreeMap::new(),
            network_socket: None,
            shared_directories: Vec::new(),
        },
        separate_launch: None,
        cache: None,
        attempt_cache: None,
        retain_passed_rootfs: false,
        retain_failed_rootfs,
        root_disks_finalized: false,
        artifact_directory: directory,
        verifier_environment: Arc::new(BTreeMap::new()),
        _network: None,
        _verifier_network: None,
    }
}

#[test]
fn cold_verifier_uses_the_prepared_environment_shell() {
    assert_eq!(verifier_shell("sh", false), "sh");
    assert_eq!(verifier_shell("bash", false), "bash");
    assert_eq!(verifier_shell("sh", true), "/bin/bash");
}

#[test]
fn guest_timezone_matches_iana_time_zone_link_parsing() {
    assert_eq!(
        timezone_from_link("/usr/share/zoneinfo//UTC").as_deref(),
        Some("/UTC")
    );
    assert_eq!(
        timezone_from_link("../usr/share/zoneinfo/America/New_York").as_deref(),
        Some("America/New_York")
    );
    assert_eq!(timezone_from_link("/not-zoneinfo/UTC"), None);
}

#[test]
fn guest_date_uses_the_guest_timezone() {
    let timestamp = Timestamp::from_second(1_774_918_800).unwrap();

    assert_eq!(current_date_at(timestamp, "Etc/UTC"), "2026-03-31");
    assert_eq!(
        current_date_at(timestamp, "America/Los_Angeles"),
        "2026-03-30"
    );
    assert_eq!(current_date_at(timestamp, "/UTC"), "2026-03-31");
}

#[test]
fn cached_verifier_omits_the_complete_pinned_uv_bootstrap() {
    assert!(CACHED_VERIFIER_SCRIPT.starts_with("/tmp/"));
    let supported = br"#!/bin/bash
# Install curl
apt-get update
apt-get install -y curl
# Install uv
curl -LsSf https://astral.sh/uv/0.9.5/install.sh | sh
source $HOME/.local/bin/env
# Check if we're in a valid working directory
uvx pytest
";
    let setup = recognized_verifier_setup(supported).unwrap();
    assert!(setup.skip_setup);
    assert_eq!(&supported[..setup.cacheable_start], b"#!/bin/bash\n");
    let omitted = &supported[setup.cacheable_start..setup.cacheable_end];
    assert!(omitted.windows(7).any(|window| window == b"apt-get"));
    assert!(omitted.windows(9).any(|window| window == b"astral.sh"));
    assert!(omitted.windows(7).any(|window| window == b"source "));
    assert!(!omitted.windows(4).any(|window| window == b"uvx "));
    let transformed = cached_verifier_script(supported, setup);
    let transformed = std::str::from_utf8(&transformed).unwrap();
    assert!(transformed.starts_with("#!/bin/bash\n"));
    assert!(!transformed.contains("apt-get"));
    assert!(transformed.contains("source /root/.local/bin/env"));
    assert!(!transformed.contains("astral.sh"));
    assert!(transformed.contains("uvx pytest"));

    assert!(recognized_verifier_setup(b"pip install pytest\npytest").is_none());
    assert!(
        recognized_verifier_setup(
            br"apt-get update
apt-get install -y curl
curl -LsSf https://astral.sh/uv/latest/install.sh | sh
source $HOME/.local/bin/env
# Check if we're in a valid working directory
"
        )
        .is_none()
    );
    let custom_setup = recognized_verifier_setup(
        br"#!/bin/bash
apt-get update
apt-get install -y curl git libgl1
curl -LsSf https://astral.sh/uv/0.9.5/install.sh | sh
source $HOME/.local/bin/env
# Check if we're in a valid working directory
",
    )
    .unwrap();
    assert!(!custom_setup.skip_setup);

    let stateful_setup = recognized_verifier_setup(
        br"#!/bin/bash
apt-get update
apt-get install -y curl
touch /root/extra-state
curl -LsSf https://astral.sh/uv/0.9.5/install.sh | sh
source $HOME/.local/bin/env
# Check if we're in a valid working directory
",
    )
    .unwrap();
    assert!(!stateful_setup.skip_setup);

    let key = verifier_cache_key(
        std::ffi::OsStr::new("rootfs.ext4"),
        omitted,
        512 * 1024 * 1024,
    );
    let different_verifier_body = supported
        .strip_suffix(b"uvx pytest\n")
        .unwrap()
        .iter()
        .copied()
        .chain(b"uvx python -m unittest\n".iter().copied())
        .collect::<Vec<_>>();
    let different_setup = recognized_verifier_setup(&different_verifier_body).unwrap();
    assert_eq!(
        key,
        verifier_cache_key(
            std::ffi::OsStr::new("rootfs.ext4"),
            &different_verifier_body
                [different_setup.cacheable_start..different_setup.cacheable_end],
            512 * 1024 * 1024,
        )
    );
}

#[test]
fn retries_only_dependency_bootstrap_network_failures() {
    let dns_failure = VmCommandOutput {
        exit_code: 0,
        stdout: b"curl: (6) Could not resolve host: astral.sh\n\
                /tests/test.sh: line 19: uvx: command not found\n"
            .to_vec(),
        stderr: Vec::new(),
    };
    assert!(verifier_bootstrap_network_failed(&dns_failure));

    let gateway_failure = VmCommandOutput {
        exit_code: 0,
        stdout: b"failed to download https://github.com/astral-sh/uv/releases/download/uv\n\
                curl: (22) The requested URL returned error: 504\n\
                /tests/test.sh: line 19: uvx: command not found\n"
            .to_vec(),
        stderr: Vec::new(),
    };
    assert!(verifier_bootstrap_network_failed(&gateway_failure));

    let apt_dns_failure = VmCommandOutput {
        exit_code: 100,
        stdout: Vec::new(),
        stderr: b"Temporary failure resolving 'deb.debian.org'\n".to_vec(),
    };
    assert!(verifier_bootstrap_network_failed(&apt_dns_failure));

    let genuine_test_failure = VmCommandOutput {
        exit_code: 0,
        stdout: b"FAILED test_outputs.py::test_data_matches\n\
                AssertionError: result.txt contains unexpected value\n"
            .to_vec(),
        stderr: Vec::new(),
    };
    assert!(!verifier_bootstrap_network_failed(&genuine_test_failure));

    let task_owned_download_failure = VmCommandOutput {
        exit_code: 0,
        stdout: b"Could not resolve host: github.com\nFAILED test_outputs.py\n".to_vec(),
        stderr: Vec::new(),
    };
    assert!(!verifier_bootstrap_network_failed(
        &task_owned_download_failure
    ));
    assert_eq!(
        (0..=4)
            .map(verifier_network_retry_delay)
            .collect::<Vec<_>>(),
        [2, 4, 8, 16, 32].map(std::time::Duration::from_secs)
    );
}

#[tokio::test]
async fn same_vm_verifier_staging_normalizes_file_and_directory_mtimes() {
    use std::os::unix::fs::PermissionsExt;

    let source = tempfile::tempdir().unwrap();
    let nested = source.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let file = source.path().join("test.sh");
    fs::write(&file, "#!/bin/sh\n").unwrap();
    fs::set_permissions(source.path(), fs::Permissions::from_mode(0o751)).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o711)).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();

    let control = tempfile::tempdir().unwrap();
    let journal = control.path().join("requests.jsonl");
    let script = r#"
request_id=0
while IFS= read -r request; do
    printf '%s\n' "$request" >> "$1"
    case "$request" in
        *'"kind":"create_directory"'*) kind=create_directory ;;
        *'"kind":"write_file"'*) kind=write_file ;;
        *'"kind":"shutdown"'*) kind=shutdown ;;
        *) exit 91 ;;
    esac
    printf '{"kind":"%s","payload":{"id":%s,"error":null}}\n' "$kind" "$request_id"
    if [ "$kind" = shutdown ]; then
        exit 0
    fi
    request_id=$((request_id + 1))
done
"#;
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("nanocodex-verifier-staging")
        .arg(&journal);
    let session = VmToolSession::spawn(&mut command).unwrap();

    VmVerifier::copy_directory(&session, source.path(), source.path(), Path::new("/tests"))
        .await
        .unwrap();
    session.shutdown().await.unwrap();

    let requests = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let writes = requests
        .iter()
        .filter(|request| request["kind"] == "write_file")
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0]["payload"]["path"], "/tests/test.sh");
    assert_eq!(writes[0]["payload"]["mode"], 0o640);
    assert_eq!(writes[0]["payload"]["modified_unix_seconds"], 0);

    for (path, final_mode) in [("/tests", 0o751), ("/tests/nested", 0o711)] {
        let creates = requests
            .iter()
            .filter(|request| {
                request["kind"] == "create_directory"
                    && request["payload"]["path"]
                        .as_str()
                        .is_some_and(|actual| Path::new(actual) == Path::new(path))
            })
            .collect::<Vec<_>>();
        assert_eq!(creates.len(), 2, "{path} must be opened then finalized");
        assert_eq!(creates[0]["payload"]["mode"], 0o700);
        assert!(creates[0]["payload"].get("modified_unix_seconds").is_none());
        assert_eq!(creates[1]["payload"]["mode"], final_mode);
        assert_eq!(creates[1]["payload"]["modified_unix_seconds"], 0);
    }
}

#[tokio::test]
async fn verifier_boundary_terminates_managed_tool_processes_before_staging() {
    let control = tempfile::tempdir().unwrap();
    let journal = control.path().join("requests.jsonl");
    let script = r#"
request_id=0
while IFS= read -r request; do
    printf '%s\n' "$request" >> "$1"
    case "$request" in
        *'"kind":"terminate_tool_processes"'*) kind=terminate_tool_processes ;;
        *'"kind":"create_directory"'*) kind=create_directory ;;
        *'"kind":"write_file"'*) kind=write_file ;;
        *'"kind":"shutdown"'*) kind=shutdown ;;
        *) exit 91 ;;
    esac
    printf '{"kind":"%s","payload":{"id":%s,"error":null}}\n' "$kind" "$request_id"
    if [ "$kind" = shutdown ]; then
        exit 0
    fi
    request_id=$((request_id + 1))
done
"#;
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("nanocodex-verifier-boundary")
        .arg(&journal);
    let session = VmToolSession::spawn(&mut command).unwrap();
    let task =
        Task::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks/write-greeting"))
            .unwrap();
    let launch = VmLaunch {
        root: VmLaunchRoot::Directory(control.path().join("root")),
        workspace: "/workspace".to_owned(),
        shell: "/bin/sh".to_owned(),
        runtime_image: control.path().join("runtime"),
        vmm: control.path().join("vmm"),
        cpus: 1,
        memory_mib: 256,
        resolver_configuration: String::new(),
        environment: BTreeMap::new(),
        network_socket: None,
        shared_directories: Vec::new(),
    };
    let mut verifier = VmVerifier {
        agent_session: Some(session),
        launch,
        separate_launch: None,
        cache: None,
        attempt_cache: None,
        retain_passed_rootfs: false,
        retain_failed_rootfs: true,
        root_disks_finalized: false,
        artifact_directory: control.path().to_path_buf(),
        verifier_environment: Arc::new(BTreeMap::new()),
        _network: None,
        _verifier_network: None,
    };

    let (_, session) = verifier.start_verifier_session(&task).await.unwrap();
    session.shutdown().await.unwrap();

    let requests = fs::read_to_string(journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests[0]["kind"], "terminate_tool_processes");
    assert!(
        requests[1..]
            .iter()
            .any(|request| request["kind"] == "write_file"),
        "verifier staging must continue in the same guest after process cleanup"
    );
}

#[test]
fn verifier_timeout_preserves_partial_output_bytes() {
    let output = verifier_timeout_output(
        Duration::from_secs(17),
        VmCommandPartialOutput {
            stdout: vec![0, 0xff, b'\n'],
            stderr: vec![0x80, b'\n'],
        },
    );

    assert_eq!(output.exit_code, 124);
    assert_eq!(output.stdout, [0, 0xff, b'\n']);
    assert_eq!(
        output.stderr,
        [
            &[0x80, b'\n'][..],
            b"\ncanonical verifier exceeded its 17s deadline; \
                  the candidate is scored with reward 0\n",
        ]
        .concat()
    );
}
