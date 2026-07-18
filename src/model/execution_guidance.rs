use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{NestedToolCall, ToolOutputBody};

const LONG_COMMAND: Duration = Duration::from_secs(120);
const LONG_COMMANDS_WARNING: Duration = Duration::from_secs(420);
const RUN_BUDGET_WARNING: Duration = Duration::from_secs(420);
const MAX_NOTES_PER_REMINDER: usize = 2;

const REPEATED_ERROR_NOTE: &str = "The same error family just repeated. Stop retrying the same command pattern; inspect the failing contract, path, or tool invocation once, then switch strategy.";
const MISSING_DEPENDENCY_NOTE: &str = "A runtime or dependency probe failed again. Stop re-checking the environment; move to the best direct implementation or the cheapest bounded fallback, then keep validation lightweight.";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GuidanceKind {
    RunBudget,
    RepeatedError,
    MissingDependency,
    LongRunningBranches,
}

impl GuidanceKind {
    const fn priority(self) -> u8 {
        match self {
            Self::RunBudget => 65,
            Self::RepeatedError => 60,
            Self::MissingDependency => 58,
            Self::LongRunningBranches => 55,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct GuidanceNote {
    kind: GuidanceKind,
    text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GuidanceReminder {
    notes: Vec<GuidanceNote>,
}

impl GuidanceReminder {
    pub(super) fn notes(&self) -> &[GuidanceNote] {
        &self.notes
    }

    pub(super) fn developer_message(&self) -> Value {
        let mut text = String::from(
            "<execution_risk>\nActive execution risks to resolve before the next step:",
        );
        for note in &self.notes {
            text.push_str("\n- ");
            text.push_str(&note.text);
        }
        text.push_str("\n</execution_risk>");
        json!({
            "type": "message",
            "role": "developer",
            "content": [{
                "type": "input_text",
                "text": text,
            }],
        })
    }
}

#[derive(Default)]
pub(super) struct ExecutionGuidance {
    shell: ShellRiskState,
    pending: Vec<GuidanceNote>,
    delivered: DeliveredHints,
}

impl ExecutionGuidance {
    pub(super) fn observe(&mut self, calls: &[NestedToolCall]) {
        for call in calls {
            let signals = self.shell.observe(call);
            if signals.repeated_error {
                self.enqueue(GuidanceKind::RepeatedError, REPEATED_ERROR_NOTE.to_owned());
            }
            if signals.missing_dependency {
                self.enqueue(
                    GuidanceKind::MissingDependency,
                    MISSING_DEPENDENCY_NOTE.to_owned(),
                );
            }
            if signals.long_running_branches {
                let seconds = self.shell.long_command_wait.as_secs();
                self.enqueue(
                    GuidanceKind::LongRunningBranches,
                    format!(
                        "Long-running command branches have consumed about {seconds}s of shell wait time. Preserve the best current candidate, avoid starting another heavy install, build, or training branch, and finish with the cheapest decisive validation."
                    ),
                );
            }
        }
    }

    pub(super) fn observe_elapsed(&mut self, elapsed: Duration) {
        if elapsed >= RUN_BUDGET_WARNING {
            let minutes = elapsed.as_secs() / 60;
            self.enqueue(
                GuidanceKind::RunBudget,
                format!(
                    "This run has consumed about {minutes} minutes. Stop opening new implementation branches; preserve the best current candidate, perform the cheapest decisive acceptance check, and finalize before the external trial budget expires."
                ),
            );
        }
    }

    pub(super) fn pending_reminder(&self) -> Option<GuidanceReminder> {
        if self.pending.is_empty() {
            return None;
        }
        let mut notes = self.pending.clone();
        notes.sort_by_key(|note| std::cmp::Reverse(note.kind.priority()));
        notes.truncate(MAX_NOTES_PER_REMINDER);
        Some(GuidanceReminder { notes })
    }

    pub(super) fn mark_delivered(&mut self, reminder: &GuidanceReminder) {
        for note in &reminder.notes {
            self.delivered.mark(note.kind);
        }
        self.pending
            .retain(|note| !reminder.notes.iter().any(|sent| sent.kind == note.kind));
    }

    fn enqueue(&mut self, kind: GuidanceKind, text: String) {
        if self.delivered.contains(kind) || self.pending.iter().any(|note| note.kind == kind) {
            return;
        }
        self.pending.push(GuidanceNote { kind, text });
    }
}

#[derive(Default)]
struct DeliveredHints(Vec<GuidanceKind>);

impl DeliveredHints {
    fn contains(&self, kind: GuidanceKind) -> bool {
        self.0.contains(&kind)
    }

    fn mark(&mut self, kind: GuidanceKind) {
        if !self.contains(kind) {
            self.0.push(kind);
        }
    }
}

#[derive(Default)]
struct ShellRiskState {
    sessions: HashMap<i64, ShellCommandObservation>,
    long_commands: u32,
    long_command_wait: Duration,
    last_error_signature: Option<String>,
    same_error_repeats: u8,
    dependency_probe_failures: u8,
}

impl ShellRiskState {
    fn observe(&mut self, call: &NestedToolCall) -> RiskSignals {
        if !matches!(call.name.as_str(), "exec_command" | "write_stdin") {
            return RiskSignals::default();
        }
        let ToolOutputBody::Text(output) = &call.output else {
            return RiskSignals::default();
        };
        let Ok(result) = serde_json::from_str::<ShellResult>(output) else {
            return RiskSignals::default();
        };
        let Ok(wait) = Duration::try_from_secs_f64(result.wall_time_seconds) else {
            return RiskSignals::default();
        };

        match call.name.as_str() {
            "exec_command" => {
                let Some(command) = call.input.get("cmd").and_then(Value::as_str) else {
                    return RiskSignals::default();
                };
                let mut observation = ShellCommandObservation::new(command);
                let update = observation.record(wait, &result.output);
                self.apply_long_update(&update);
                if let Some(session_id) = result.session_id {
                    self.sessions.insert(session_id, observation);
                    self.long_signal()
                } else {
                    self.finish(&observation, result.exit_code)
                }
            }
            "write_stdin" => {
                let Some(session_id) = call.input.get("session_id").and_then(Value::as_i64) else {
                    return RiskSignals::default();
                };
                let Some(mut observation) = self.sessions.remove(&session_id) else {
                    return RiskSignals::default();
                };
                let update = observation.record(wait, &result.output);
                self.apply_long_update(&update);
                if result.session_id.is_some() {
                    self.sessions.insert(session_id, observation);
                    self.long_signal()
                } else {
                    self.finish(&observation, result.exit_code)
                }
            }
            _ => RiskSignals::default(),
        }
    }

    fn apply_long_update(&mut self, update: &LongUpdate) {
        if update.crossed_threshold {
            self.long_commands = self.long_commands.saturating_add(1);
        }
        self.long_command_wait = self.long_command_wait.saturating_add(update.long_wait);
    }

    fn finish(
        &mut self,
        observation: &ShellCommandObservation,
        exit_code: Option<i32>,
    ) -> RiskSignals {
        let repeated_error = if exit_code.is_some_and(|code| code != 0) {
            let signature = observation.error_signature(exit_code);
            if self.last_error_signature.as_ref() == Some(&signature) {
                self.same_error_repeats = self.same_error_repeats.saturating_add(1);
            } else {
                self.last_error_signature = Some(signature);
                self.same_error_repeats = 1;
            }
            self.same_error_repeats >= 2
        } else {
            self.last_error_signature = None;
            self.same_error_repeats = 0;
            false
        };

        let missing_dependency =
            if observation.dependency_probe && observation.saw_missing_dependency {
                self.dependency_probe_failures = self.dependency_probe_failures.saturating_add(1);
                self.dependency_probe_failures >= 2
            } else {
                self.dependency_probe_failures = 0;
                false
            };

        RiskSignals {
            repeated_error,
            missing_dependency,
            long_running_branches: self.long_warning_active(),
        }
    }

    fn long_signal(&self) -> RiskSignals {
        RiskSignals {
            long_running_branches: self.long_warning_active(),
            ..RiskSignals::default()
        }
    }

    fn long_warning_active(&self) -> bool {
        self.long_commands >= 2 && self.long_command_wait >= LONG_COMMANDS_WARNING
    }
}

#[derive(Default)]
struct RiskSignals {
    repeated_error: bool,
    missing_dependency: bool,
    long_running_branches: bool,
}

struct ShellCommandObservation {
    dependency_probe: bool,
    wait: Duration,
    counted_long: bool,
    error_line: Option<String>,
    last_line: Option<String>,
    saw_missing_dependency: bool,
}

impl ShellCommandObservation {
    fn new(command: &str) -> Self {
        Self {
            dependency_probe: looks_like_dependency_probe(command),
            wait: Duration::ZERO,
            counted_long: false,
            error_line: None,
            last_line: None,
            saw_missing_dependency: false,
        }
    }

    fn record(&mut self, wait: Duration, output: &str) -> LongUpdate {
        self.wait = self.wait.saturating_add(wait);
        let was_long = self.counted_long;
        if !was_long && self.wait >= LONG_COMMAND {
            self.counted_long = true;
        }

        for line in output.lines().filter(|line| !line.trim().is_empty()) {
            let normalized = normalize_signature(line);
            if is_error_line(&normalized) && self.error_line.is_none() {
                self.error_line = Some(normalized.clone());
            }
            self.last_line = Some(normalized);
        }
        self.saw_missing_dependency |= has_missing_dependency(output);

        LongUpdate {
            crossed_threshold: !was_long && self.counted_long,
            long_wait: if was_long {
                wait
            } else if self.counted_long {
                self.wait
            } else {
                Duration::ZERO
            },
        }
    }

    fn error_signature(&self, exit_code: Option<i32>) -> String {
        let detail = self
            .error_line
            .as_deref()
            .or(self.last_line.as_deref())
            .unwrap_or("no output");
        format!("exit:{}:{detail}", exit_code.unwrap_or_default())
    }
}

struct LongUpdate {
    crossed_threshold: bool,
    long_wait: Duration,
}

#[derive(Deserialize)]
struct ShellResult {
    wall_time_seconds: f64,
    exit_code: Option<i32>,
    session_id: Option<i64>,
    output: String,
}

fn normalize_signature(line: &str) -> String {
    let mut normalized = String::new();
    let mut in_digits = false;
    let mut in_whitespace = false;
    for character in line.trim().chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                normalized.push('#');
            }
            in_digits = true;
            in_whitespace = false;
        } else if character.is_whitespace() {
            if !normalized.is_empty() && !in_whitespace {
                normalized.push(' ');
            }
            in_digits = false;
            in_whitespace = true;
        } else {
            normalized.extend(character.to_lowercase());
            in_digits = false;
            in_whitespace = false;
        }
        if normalized.chars().count() >= 180 {
            break;
        }
    }
    normalized.trim_end().to_owned()
}

fn is_error_line(line: &str) -> bool {
    [
        "error",
        "exception",
        "traceback",
        "not found",
        "failed",
        "cannot",
        "no such file",
        "undefined reference",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

fn has_missing_dependency(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    [
        "command not found",
        "no module named",
        "modulenotfounderror",
        "not installed",
        "not found",
        "unable to locate package",
        "could not find package",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

fn looks_like_dependency_probe(command: &str) -> bool {
    let command = command.trim().to_ascii_lowercase();
    [
        "command -v",
        "which ",
        "type ",
        "--version",
        "python -c",
        "python3 -c",
        "pip ",
        "pip3 ",
        "uv ",
        "apt-cache",
        "apt-get",
        "dnf ",
        "apk ",
    ]
    .iter()
    .any(|marker| command.contains(marker))
        || ["ls ", "stat ", "file ", "test -", "find "]
            .iter()
            .any(|prefix| command.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{ExecutionGuidance, GuidanceKind, LONG_COMMAND};
    use crate::tools::{NestedToolCall, ToolOutputBody};

    #[test]
    fn repeated_error_is_delivered_once() {
        let mut guidance = ExecutionGuidance::default();
        guidance.observe(&[
            completed_exec("build", 1.0, 7, "Error: source line 12 failed"),
            completed_exec("build", 1.0, 7, "Error: source line 98 failed"),
        ]);

        let reminder = guidance
            .pending_reminder()
            .expect("repeated error reminder");
        assert_eq!(reminder.notes.len(), 1);
        assert_eq!(reminder.notes[0].kind, GuidanceKind::RepeatedError);
        guidance.mark_delivered(&reminder);

        guidance.observe(&[
            completed_exec("build", 1.0, 7, "Error: source line 99 failed"),
            completed_exec("build", 1.0, 7, "Error: source line 100 failed"),
        ]);
        assert!(guidance.pending_reminder().is_none());
    }

    #[test]
    fn dependency_probe_requires_two_consecutive_failures() {
        let mut guidance = ExecutionGuidance::default();
        guidance.observe(&[completed_exec(
            "python -c 'import alpha'",
            1.0,
            1,
            "ModuleNotFoundError: No module named 'alpha'",
        )]);
        guidance.observe(&[completed_exec("true", 1.0, 0, "")]);
        guidance.observe(&[completed_exec(
            "python -c 'import beta'",
            1.0,
            1,
            "ModuleNotFoundError: No module named 'beta'",
        )]);
        assert!(guidance.pending_reminder().is_none());

        guidance.observe(&[completed_exec(
            "python -c 'import gamma'",
            1.0,
            1,
            "ModuleNotFoundError: No module named 'gamma'",
        )]);
        let reminder = guidance
            .pending_reminder()
            .expect("missing dependency reminder");
        assert_eq!(reminder.notes.len(), 1);
        assert_eq!(reminder.notes[0].kind, GuidanceKind::MissingDependency);
    }

    #[test]
    fn long_polls_are_aggregated_by_logical_session() {
        let mut guidance = ExecutionGuidance::default();
        guidance.observe(&[pending_exec("first", 1, 30.0)]);
        guidance.observe(&[pending_write(1, 30.0)]);
        guidance.observe(&[pending_write(1, 30.0)]);
        guidance.observe(&[pending_write(1, 30.0)]);
        assert_eq!(guidance.shell.long_commands, 1);
        assert_eq!(guidance.shell.long_command_wait, LONG_COMMAND);
        assert!(guidance.pending_reminder().is_none());

        guidance.observe(&[pending_exec("second", 2, 120.0)]);
        assert_eq!(guidance.shell.long_commands, 2);
        assert!(guidance.pending_reminder().is_none());

        guidance.observe(&[pending_write(1, 180.0)]);
        let reminder = guidance.pending_reminder().expect("long branch reminder");
        assert_eq!(reminder.notes.len(), 1);
        assert_eq!(reminder.notes[0].kind, GuidanceKind::LongRunningBranches);
    }

    #[test]
    fn elapsed_run_budget_is_delivered_once() {
        let mut guidance = ExecutionGuidance::default();
        guidance.observe_elapsed(Duration::from_secs(419));
        assert!(guidance.pending_reminder().is_none());

        guidance.observe_elapsed(Duration::from_secs(420));
        let reminder = guidance.pending_reminder().expect("run budget reminder");
        assert_eq!(reminder.notes.len(), 1);
        assert_eq!(reminder.notes[0].kind, GuidanceKind::RunBudget);
        assert!(reminder.notes[0].text.contains("about 7 minutes"));
        guidance.mark_delivered(&reminder);

        guidance.observe_elapsed(Duration::from_secs(900));
        assert!(guidance.pending_reminder().is_none());
    }

    #[test]
    fn a_reminder_contains_at_most_two_notes() {
        let mut guidance = ExecutionGuidance::default();
        guidance.enqueue(GuidanceKind::LongRunningBranches, "long".to_owned());
        guidance.enqueue(GuidanceKind::MissingDependency, "dependency".to_owned());
        guidance.enqueue(GuidanceKind::RepeatedError, "error".to_owned());

        let first = guidance.pending_reminder().expect("first reminder");
        assert_eq!(first.notes.len(), 2);
        assert_eq!(first.notes[0].kind, GuidanceKind::RepeatedError);
        assert_eq!(first.notes[1].kind, GuidanceKind::MissingDependency);
        let message = first.developer_message();
        assert_eq!(message["role"], "developer");
        assert!(message.to_string().contains("<execution_risk>"));
        guidance.mark_delivered(&first);

        let second = guidance.pending_reminder().expect("remaining reminder");
        assert_eq!(second.notes.len(), 1);
        assert_eq!(second.notes[0].kind, GuidanceKind::LongRunningBranches);
    }

    fn completed_exec(
        command: &str,
        wall_time_seconds: f64,
        exit_code: i32,
        output: &str,
    ) -> NestedToolCall {
        shell_call(
            "exec_command",
            json!({ "cmd": command }),
            &json!({
                "wall_time_seconds": wall_time_seconds,
                "exit_code": exit_code,
                "output": output,
            }),
        )
    }

    fn pending_exec(command: &str, session_id: i64, wall_time_seconds: f64) -> NestedToolCall {
        shell_call(
            "exec_command",
            json!({ "cmd": command }),
            &json!({
                "wall_time_seconds": wall_time_seconds,
                "session_id": session_id,
                "output": "",
            }),
        )
    }

    fn pending_write(session_id: i64, wall_time_seconds: f64) -> NestedToolCall {
        shell_call(
            "write_stdin",
            json!({ "session_id": session_id }),
            &json!({
                "wall_time_seconds": wall_time_seconds,
                "session_id": session_id,
                "output": "",
            }),
        )
    }

    fn shell_call(
        name: &str,
        input: serde_json::Value,
        output: &serde_json::Value,
    ) -> NestedToolCall {
        NestedToolCall {
            call_id: "nested".to_owned(),
            name: name.to_owned(),
            input,
            output: ToolOutputBody::Text(output.to_string()),
            success: true,
            duration_ns: 0,
            metadata: None,
        }
    }
}
