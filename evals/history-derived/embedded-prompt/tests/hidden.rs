use embedded_prompt::{diagnostic_prompt, system_prompt};

const EXPECTED: &str = "You are a careful coding agent.\n\nInspect the repository before editing.\nRun focused checks after changes.\n\nFinish with a concise implementation summary.\n";

#[test]
fn exact_prompt_contract_is_preserved() {
    assert_eq!(system_prompt(), EXPECTED);
    assert_eq!(diagnostic_prompt(), EXPECTED);
}
