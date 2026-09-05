use std::{path::PathBuf, time::Duration};

use nanocodex_eval::{
    Resources, ScoringPolicy,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use serde::Serialize;

use crate::{sha256_file, sha256_values};

const LS20_BASELINE_ACTIONS: [u64; 7] = [22, 123, 73, 84, 96, 192, 186];
const TN36_BASELINE_ACTIONS: [u64; 7] = [32, 72, 26, 40, 30, 55, 62];
const SB26_BASELINE_ACTIONS: [u64; 8] = [18, 28, 18, 19, 31, 23, 58, 18];
const SMOKE_GAMES: [ArcAgi3Smoke; 3] = [
    ArcAgi3Smoke::new("ls20-9607627b", "LS20", "keyboard", &LS20_BASELINE_ACTIONS),
    ArcAgi3Smoke::new("tn36-ef4dde99", "TN36", "click", &TN36_BASELINE_ACTIONS),
    ArcAgi3Smoke::new(
        "sb26-7fbdac44",
        "SB26",
        "keyboard_click",
        &SB26_BASELINE_ACTIONS,
    ),
];
const SMOKE_ACTION_CAP: u64 = 3;

#[derive(Clone, Copy, Debug)]
struct ArcAgi3Smoke {
    game_id: &'static str,
    title: &'static str,
    controls: &'static str,
    baseline_actions: &'static [u64],
}

impl ArcAgi3Smoke {
    const fn new(
        game_id: &'static str,
        title: &'static str,
        controls: &'static str,
        baseline_actions: &'static [u64],
    ) -> Self {
        Self {
            game_id,
            title,
            controls,
            baseline_actions,
        }
    }
}

/// ARC Prize's public ARC-AGI-3 interactive environment contract.
#[derive(Clone, Debug)]
pub struct ArcAgi3 {
    benchmarking: PathBuf,
    toolkit: PathBuf,
    revision: String,
    environment: Environment,
    harness: Harness,
}

impl ArcAgi3 {
    /// Creates the public interactive smoke importer from pinned official checkouts.
    #[must_use]
    pub fn new(
        benchmarking: impl Into<PathBuf>,
        toolkit: impl Into<PathBuf>,
        revision: impl Into<String>,
        environment: Environment,
        harness: Harness,
    ) -> Self {
        Self {
            benchmarking: benchmarking.into(),
            toolkit: toolkit.into(),
            revision: revision.into(),
            environment,
            harness,
        }
    }

    fn provenance_digest(&self) -> Result<String, ImportError> {
        let files = [
            self.benchmarking.join("benchmarking/agent.py"),
            self.benchmarking.join("benchmarking/base.py"),
            self.benchmarking.join("benchmarking/model_configs.yaml"),
            self.toolkit.join("arc_agi/remote_wrapper.py"),
            self.toolkit.join("arc_agi/scorecard.py"),
        ];
        let digests = files
            .iter()
            .map(|path| sha256_file(path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(sha256_values(digests.iter().map(String::as_bytes)))
    }
}

impl DatasetImporter for ArcAgi3 {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let source = SourceIdentity::new(
            "arc-prize-arc-agi-3",
            &self.revision,
            self.provenance_digest()?,
        )?;
        let instructions = "You are playing a game. Your goal is to win. Include any context you want to carry forward in your reasoning, then execute exactly one available game action at a time with the arc-agi-3 command. The final action executed in each step is the action scored by the environment. This bounded shell-tool topology validates adapter plumbing only; it is not the official per-frame model-call harness.";
        let mut plan = DatasetPlan::new("arc-agi-3-public-smoke", source)?;
        for smoke in SMOKE_GAMES {
            let case = ArcAgi3Case {
                game_id: smoke.game_id,
                title: smoke.title,
                controls: smoke.controls,
                baseline_actions: smoke.baseline_actions,
                action_budget_multiplier: 5.0,
                smoke_action_cap: SMOKE_ACTION_CAP,
                max_animation_frames: 7,
                api_base_url: "https://three.arcprize.org",
                score_contract: "official ARC-AGI-3 environment scorecard (smoke evidence only)",
                benchmark_revision: &self.revision,
            };
            let case_json = serde_json::to_vec_pretty(&case).map_err(|error| {
                ImportError::Invalid(format!("failed to encode ARC-AGI-3 case: {error}"))
            })?;
            let prompt = format!(
                "Play the public ARC-AGI-3 game {} ({}, {} controls) and maximize its official score. Start by running `arc-agi-3 observe`. Then use `arc-agi-3 act ACTION1` through `ACTION7`, or `arc-agi-3 act ACTION6 X Y` when coordinates are offered. Read every returned frame, continue until the tool reports TERMINAL, and do not inspect or modify the tool's private session files. This adapter-smoke task deliberately stops after {SMOKE_ACTION_CAP} submitted actions; it validates the interactive protocol and is not a headline ARC-AGI-3 score.",
                smoke.game_id, smoke.title, smoke.controls
            );
            let task = CasePlan::hermetic(
                smoke.game_id,
                prompt,
                self.environment.clone(),
                self.harness.clone(),
            )?
            .instructions(instructions)
            .benchmark_case_type(format!("interactive-public-smoke/{}", smoke.controls))
            .resources(Resources {
                cpus: 2,
                memory_mb: 2_048,
                storage_mb: 2_048,
                gpus: 0,
            })
            .timeouts(Duration::from_secs(900), Duration::from_secs(120))
            .scoring_policy(ScoringPolicy::AllRewardsPositive)
            .allow_internet(true)
            .environment_file("arc_case.json", case_json.clone(), 0o644)?
            .harness_file("case.json", case_json, 0o600)?;
            plan = plan.case(task);
        }
        Ok(plan)
    }
}

#[derive(Serialize)]
struct ArcAgi3Case<'a> {
    game_id: &'a str,
    title: &'a str,
    controls: &'a str,
    baseline_actions: &'a [u64],
    action_budget_multiplier: f64,
    smoke_action_cap: u64,
    max_animation_frames: u64,
    api_base_url: &'a str,
    score_contract: &'a str,
    benchmark_revision: &'a str,
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use nanocodex_eval::{
        NetworkPolicy,
        import::{Environment, Harness, ImportStore},
    };
    use tempfile::tempdir;

    use super::{ArcAgi3, SMOKE_ACTION_CAP, SMOKE_GAMES};

    #[test]
    fn imports_a_bounded_interactive_smoke_without_claiming_an_official_score() {
        let source = tempdir().unwrap();
        make_official_fixture(source.path());
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/arc-agi-3");
        let store = tempdir().unwrap();
        let dataset = ImportStore::new(store.path())
            .import(&ArcAgi3::new(
                source.path().join("benchmarking"),
                source.path().join("toolkit"),
                "benchmark@abc+toolkit@def",
                Environment::Dockerfile(assets.join("environment")),
                Harness::directory(assets.join("verifier")).unwrap(),
            ))
            .unwrap();

        assert_eq!(dataset.tasks().len(), SMOKE_GAMES.len());
        for (task, smoke) in dataset.tasks().iter().zip(SMOKE_GAMES) {
            assert_eq!(task.name(), smoke.game_id);
            assert!(task.prompt().contains("not a headline ARC-AGI-3 score"));
            assert!(task.prompt().contains(&SMOKE_ACTION_CAP.to_string()));
            assert!(task.prompt().contains(smoke.controls));
            assert_eq!(task.network(), NetworkPolicy::Public);
            let case: serde_json::Value =
                serde_json::from_slice(&fs::read(task.root().join("tests/case.json")).unwrap())
                    .unwrap();
            assert_eq!(case["game_id"], smoke.game_id);
            assert_eq!(
                case["baseline_actions"],
                serde_json::json!(smoke.baseline_actions)
            );
            assert_eq!(case["controls"], smoke.controls);
            assert_eq!(case["action_budget_multiplier"], 5.0);
            assert_eq!(case["smoke_action_cap"], SMOKE_ACTION_CAP);
        }
    }

    #[test]
    fn task_owned_driver_self_test_covers_action_and_frame_validation() {
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/arc-agi-3/environment/arc_agi_3.py");
        let output = Command::new("python3")
            .arg(script)
            .arg("self-test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "ok");
    }

    fn make_official_fixture(root: &Path) {
        for path in [
            "benchmarking/benchmarking/agent.py",
            "benchmarking/benchmarking/base.py",
            "benchmarking/benchmarking/model_configs.yaml",
            "toolkit/arc_agi/remote_wrapper.py",
            "toolkit/arc_agi/scorecard.py",
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "official fixture\n").unwrap();
        }
    }
}
