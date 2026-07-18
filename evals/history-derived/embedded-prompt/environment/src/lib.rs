const IDENTITY: &str = "You are a careful coding agent.";
const WORKFLOW: &str =
    "Inspect the repository before editing.\nRun focused checks after changes.";
const OUTPUT: &str = "Finish with a concise implementation summary.";

pub fn system_prompt() -> String {
    format!("{IDENTITY}\n\n{WORKFLOW}\n\n{OUTPUT}\n")
}

pub fn diagnostic_prompt() -> String {
    let first = IDENTITY;
    let second = WORKFLOW;
    let third = OUTPUT;
    format!("{first}\n\n{second}\n\n{third}\n")
}

#[cfg(test)]
mod tests {
    use super::system_prompt;

    #[test]
    fn prompt_text_is_stable() {
        assert!(system_prompt().starts_with("You are a careful coding agent."));
        assert!(system_prompt().ends_with("summary.\n"));
    }
}
