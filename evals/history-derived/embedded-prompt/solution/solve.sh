#!/bin/sh
set -eu

cat > /app/src/system.md <<'EOF'
You are a careful coding agent.

Inspect the repository before editing.
Run focused checks after changes.

Finish with a concise implementation summary.
EOF

cat > /app/src/lib.rs <<'EOF'
const SYSTEM_PROMPT: &str = include_str!("system.md");

pub fn system_prompt() -> String {
    SYSTEM_PROMPT.to_owned()
}

pub fn diagnostic_prompt() -> String {
    SYSTEM_PROMPT.to_owned()
}

#[cfg(test)]
mod tests {
    use super::system_prompt;

    #[test]
    fn prompt_text_is_stable() {
        assert_eq!(system_prompt(), include_str!("system.md"));
    }
}
EOF

cargo test --quiet --manifest-path /app/Cargo.toml
