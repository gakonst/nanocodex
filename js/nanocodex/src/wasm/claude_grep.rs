//! Bounded Rust regex matching for Node's host-owned Claude filesystem adapter.
//! Keep the same RegexBuilder flags and str::lines semantics as the native Grep.

use regex::{Regex, RegexBuilder};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use super::js_error;

const MAX_FILE: usize = 1024 * 1024;
const MAX_RESULT: usize = 16 * MAX_FILE;

#[wasm_bindgen(js_name = ClaudeGrepRegex)]
pub struct WasmClaudeGrepRegex {
    regex: Regex,
    multiline: bool,
}

#[derive(Serialize)]
struct Matches<'a> {
    hits: Vec<usize>,
    occurrences: Vec<(usize, &'a str)>,
}

#[wasm_bindgen(js_class = ClaudeGrepRegex)]
impl WasmClaudeGrepRegex {
    #[wasm_bindgen(constructor)]
    pub fn new(pattern: &str, case_insensitive: bool, multiline: bool) -> Result<Self, JsValue> {
        if pattern.is_empty() || pattern.len() > 4096 {
            return Err(js_error("pattern must be 1 to 4096 bytes"));
        }
        let regex = RegexBuilder::new(pattern)
            .case_insensitive(case_insensitive)
            .multi_line(multiline)
            .dot_matches_new_line(multiline)
            .size_limit(4 * MAX_FILE)
            .build()
            .map_err(|error| js_error(format!("invalid or oversized regex: {error}")))?;
        Ok(Self { regex, multiline })
    }

    /// Return zero-based matching lines and, when requested, per-line matches.
    /// No filesystem access or JavaScript regex evaluation occurs in this layer.
    pub fn scan(&self, contents: &str, only_matching: bool) -> Result<String, JsValue> {
        if contents.len() > MAX_FILE {
            return Err(js_error("file exceeds 1 MiB text limit"));
        }
        if only_matching && self.multiline {
            return Err(js_error("-o cannot be combined with multiline"));
        }
        let lines: Vec<&str> = contents.lines().collect();
        let mut hits = vec![false; lines.len()];
        let mut occurrences = Vec::new();
        if self.multiline {
            let starts: Vec<usize> = std::iter::once(0)
                .chain(contents.match_indices('\n').map(|(index, _)| index + 1))
                .collect();
            for found in self.regex.find_iter(contents) {
                let first = starts
                    .partition_point(|&start| start <= found.start())
                    .saturating_sub(1);
                let last_byte = found.end().saturating_sub(1).max(found.start());
                let last = starts
                    .partition_point(|&start| start <= last_byte)
                    .saturating_sub(1);
                for hit in hits.iter_mut().take(last.saturating_add(1)).skip(first) {
                    *hit = true;
                }
            }
        } else {
            let mut result_bound = 0usize;
            for (index, line) in lines.iter().enumerate() {
                if only_matching {
                    for found in self.regex.find_iter(line) {
                        // JSON escaping takes at most six bytes per UTF-8 byte;
                        // reserve tuple/index overhead before allocating results.
                        result_bound = result_bound.saturating_add(32 + 6 * found.len());
                        if result_bound > MAX_RESULT {
                            return Err(js_error("regex result exceeds 16 MiB search bound"));
                        }
                        hits[index] = true;
                        occurrences.push((index, found.as_str()));
                    }
                } else {
                    hits[index] = self.regex.is_match(line);
                }
            }
        }
        let result = Matches {
            hits: hits
                .iter()
                .enumerate()
                .filter_map(|(index, &hit)| hit.then_some(index))
                .collect(),
            occurrences,
        };
        let json = serde_json::to_string(&result).map_err(js_error)?;
        if json.len() > MAX_RESULT {
            return Err(js_error("regex result exceeds 16 MiB search bound"));
        }
        Ok(json)
    }
}
