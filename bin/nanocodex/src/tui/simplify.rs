const WORKFLOW: &str = r#"Improve the quality of the recently changed code without changing its intended behavior. This is a cleanup pass, not a correctness or bug-hunting review.

Determine the review scope from the current Git diff. Prefer the branch diff against its upstream. If no upstream exists, use the repository's main branch or the previous commit. Include uncommitted changes when present, and use them when the branch diff is empty.

If no changed code remains after those fallbacks, report that there is nothing to simplify and stop. Otherwise, call `simplify_review` exactly once with the unified diff and the optional focus from this request. The tool uses the session's canonical subagent runtime to run four independent read-only reviewers concurrently for reuse, simplification, efficiency, and abstraction-depth issues.

Wait for the review reports, deduplicate findings that identify the same line or mechanism, and inspect the cited code before acting. Apply each valid, behavior-preserving cleanup directly. Skip and report findings that are false positives, change intended behavior, or require work well outside the reviewed diff. Run focused validation for the files you change. Finish with a compact summary of fixes and skips, or confirm that the changed code was already clean."#;

pub(super) fn prompt(focus: Option<&str>) -> String {
    focus.map_or_else(
        || WORKFLOW.to_owned(),
        |focus| format!("Additional review focus: {focus}\n\n{WORKFLOW}"),
    )
}

#[cfg(test)]
mod tests {
    use super::prompt;

    #[test]
    fn focus_is_added_without_replacing_the_cleanup_contract() {
        let prompt = prompt(Some("memory efficiency"));

        assert!(prompt.starts_with("Additional review focus: memory efficiency"));
        assert!(prompt.contains("call `simplify_review` exactly once"));
        assert!(prompt.contains("canonical subagent runtime"));
        assert!(prompt.contains("four independent read-only reviewers concurrently"));
        assert!(prompt.contains("without changing its intended behavior"));
    }
}
