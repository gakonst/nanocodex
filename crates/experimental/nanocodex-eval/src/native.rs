use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use chrono::Utc;
use tokio::{process::Command, time::timeout};

use crate::{CleanupPhase, EvalError, PhaseTiming, Task, TaskOutput, VerifierResult};

pub(crate) struct AttemptPaths {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub verifier: PathBuf,
    pub tests: PathBuf,
    pub verifier_output: PathBuf,
    pub reward: PathBuf,
}

pub(crate) struct NativeAttempt {
    pub paths: AttemptPaths,
    pub setup_timing: PhaseTiming,
}

pub(crate) struct VerifierExecution {
    pub result: VerifierResult,
    pub timing: PhaseTiming,
    pub stdout: String,
    pub stderr: String,
    pub cleanup: CleanupPhase,
}

impl NativeAttempt {
    pub fn prepare(output: &Path, trial_name: &str, task: &Task) -> Result<Self, EvalError> {
        let started_at = Utc::now();
        let root = output.join(trial_name);
        let workspace = root.join("workspace");
        let verifier = root.join("verifier");
        let tests = root.join("tests");
        Self::create_directory(&workspace)?;
        Self::create_directory(&verifier)?;
        Self::create_directory(&tests)?;
        task.materialize_environment(&workspace)?;
        task.materialize_verifier_files(&tests)?;

        Ok(Self {
            paths: AttemptPaths {
                verifier_output: verifier.join("test-stdout.txt"),
                reward: verifier.join("reward.txt"),
                root,
                workspace,
                verifier,
                tests,
            },
            setup_timing: PhaseTiming {
                started_at,
                finished_at: Utc::now(),
            },
        })
    }

    pub async fn verify(
        &self,
        task: &Task,
        final_message: Option<&str>,
    ) -> Result<VerifierExecution, EvalError> {
        let started_at = Utc::now();
        if task.output() == TaskOutput::FinalMessage {
            fs::write(
                self.paths.workspace.join("answer.txt"),
                final_message.unwrap_or_default(),
            )?;
        }
        let mut command = Command::new("/bin/sh");
        command
            .arg(self.paths.tests.join("test.sh"))
            .current_dir(&self.paths.workspace)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.paths.workspace)
            .env("NANOCODEX_EVAL_WORKSPACE", &self.paths.workspace)
            .env("NANOCODEX_EVAL_VERIFIER_LOGS", &self.paths.verifier)
            // Retained tasks from the temporary Nanoeval repository still
            // consume these names.
            .env("NANOEVAL_WORKSPACE", &self.paths.workspace)
            .env("NANOEVAL_VERIFIER_LOGS", &self.paths.verifier)
            .envs(task.environment().iter())
            .envs(task.verifier().environment().iter())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = timeout(task.verifier().timeout(), command.output())
            .await
            .map_err(|_| EvalError::VerifierTimeout(task.verifier().timeout()))??;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let combined = match (stdout.is_empty(), stderr.is_empty()) {
            (_, true) => stdout.clone(),
            (true, false) => stderr.clone(),
            (false, false) => format!("{stdout}\n{stderr}"),
        };
        fs::write(&self.paths.verifier_output, combined)?;

        let reward_json = self.paths.verifier.join("reward.json");
        let rewards = if reward_json.is_file() {
            serde_json::from_slice::<BTreeMap<String, f64>>(&fs::read(reward_json)?)?
        } else {
            let reward_text = fs::read_to_string(&self.paths.reward)?;
            let reward = reward_text.trim().parse::<f64>()?;
            BTreeMap::from([("reward".to_owned(), reward)])
        };

        Ok(VerifierExecution {
            result: VerifierResult {
                exit_code: output.status.code().unwrap_or(1),
                rewards,
            },
            timing: PhaseTiming {
                started_at,
                finished_at: Utc::now(),
            },
            stdout,
            stderr,
            cleanup: CleanupPhase::not_required(),
        })
    }

    fn create_directory(path: &Path) -> Result<(), EvalError> {
        fs::create_dir_all(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::NativeAttempt;
    use crate::Task;

    #[tokio::test]
    async fn prepares_and_verifies_an_independent_native_workspace() {
        let task_directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks/write-greeting");
        let task = Task::load(task_directory).unwrap();
        let output = tempdir().unwrap();
        let attempt = NativeAttempt::prepare(output.path(), "trial", &task).unwrap();

        fs::write(
            attempt.paths.workspace.join("greeting.txt"),
            "hello from nanoeval\n",
        )
        .unwrap();
        let execution = attempt.verify(&task, None).await.unwrap();

        assert!((execution.result.rewards["reward"] - 1.0).abs() < f64::EPSILON);
        assert_eq!(execution.result.exit_code, 0);
        assert!(attempt.paths.verifier_output.is_file());
        assert_eq!(fs::read_to_string(attempt.paths.reward).unwrap(), "1\n");
    }
}
