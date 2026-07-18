#!/bin/sh
set -eu

python3 - <<'PY'
from pathlib import Path

path = Path("/app/src/lib.rs")
source = path.read_text()
old = '''    pub fn build(&self, reconnected: bool) -> RequestEnvelope {
        let _ = reconnected;
        RequestEnvelope {
            previous_response_id: self.previous_response_id.clone(),
            prompt_cache_key: self.prompt_cache_key.clone(),
            items: self.history.clone(),
        }
    }
'''
new = '''    pub fn build(&self, reconnected: bool) -> RequestEnvelope {
        let (previous_response_id, items) = if reconnected {
            let items = self
                .history
                .iter()
                .cloned()
                .map(|mut item| {
                    item.id = None;
                    item
                })
                .collect();
            (None, items)
        } else {
            (
                self.previous_response_id.clone(),
                self.history[self.delta_start..].to_vec(),
            )
        };

        RequestEnvelope {
            previous_response_id,
            prompt_cache_key: self.prompt_cache_key.clone(),
            items,
        }
    }
'''
assert old in source
path.write_text(source.replace(old, new))
PY

cargo test --quiet --manifest-path /app/Cargo.toml
