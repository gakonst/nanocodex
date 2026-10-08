//! Rejects agent commands whose purpose is idle waiting.
//!
//! Models tend to "wait" for builds or CI with `sleep 240; tail log` or
//! `gh run watch`, which pins a Hand process and burns wall time without
//! progress. Long-running work should instead be started once and observed by
//! polling its retained session (`write_stdin` with empty input).

/// Total foreground `sleep` seconds tolerated in one command.
pub(crate) const MAX_SLEEP_SECONDS: f64 = 10.0;

pub(crate) fn rejection(script: &str) -> Option<String> {
    let lower = script.to_ascii_lowercase();
    if lower.contains("gh run watch")
        || (lower.contains("gh pr checks") && lower.contains("--watch"))
    {
        return Some(
            "exec_command rejected: blocking CI watchers (`gh run watch`, `gh pr checks --watch`) are not allowed. Check status once with `gh run list`/`gh pr checks` and continue other work."
                .to_owned(),
        );
    }
    let looping = has_loop(&lower) && sleep_seconds(script) >= 1.0;
    (sleep_then_check(script) || looping).then(|| {
        "exec_command rejected: idle waiting with `sleep` (long or inside a polling loop) is not allowed. Start the long-running command once, then poll its retained session with write_stdin and empty chars, or do other work and check back later."
            .to_owned()
    })
}

fn has_loop(lower: &str) -> bool {
    words(lower).any(|word| word == "while" || word == "until")
}

fn words(script: &str) -> impl Iterator<Item = &str> {
    script
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '{' | '}')
        })
        .filter(|word| !word.is_empty())
}

fn sleep_seconds(script: &str) -> f64 {
    let mut total = 0.0;
    let mut tokens = words(script).peekable();
    while let Some(token) = tokens.next() {
        if token.rsplit('/').next() != Some("sleep") {
            continue;
        }
        while let Some(arg) = tokens.peek() {
            match parse_duration(arg) {
                Some(seconds) => {
                    total += seconds;
                    tokens.next();
                }
                None => break,
            }
        }
    }
    total
}

/// A foreground sleep of more than [`MAX_SLEEP_SECONDS`] followed by another
/// command is the "sleep, then look at the log" polling idiom. A trailing or
/// backgrounded sleep keeps a session alive and is left alone.
fn sleep_then_check(script: &str) -> bool {
    let mut pending = 0.0;
    for segment in script
        .split(['\n', ';'])
        .flat_map(|part| part.split("&&"))
        .map(|part| part.trim().trim_start_matches(['(', '{']).trim())
        .filter(|part| !part.is_empty())
    {
        let mut words = segment.split_whitespace();
        let is_sleep = !segment.contains('&')
            && words.next().and_then(|w| w.rsplit('/').next()) == Some("sleep");
        if is_sleep {
            pending += words.filter_map(parse_duration).sum::<f64>();
        } else if pending > MAX_SLEEP_SECONDS {
            return true;
        } else {
            pending = 0.0;
        }
    }
    false
}

fn parse_duration(arg: &str) -> Option<f64> {
    let (number, scale) = match arg.as_bytes().last()? {
        b's' => (&arg[..arg.len() - 1], 1.0),
        b'm' => (&arg[..arg.len() - 1], 60.0),
        b'h' => (&arg[..arg.len() - 1], 3600.0),
        b'd' => (&arg[..arg.len() - 1], 86400.0),
        _ => (arg, 1.0),
    };
    if arg == "infinity" {
        return Some(f64::INFINITY);
    }
    number
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v * scale)
}

#[cfg(test)]
mod tests {
    use super::rejection;

    #[test]
    fn rejects_idle_waits() {
        for script in [
            "sleep 240; tail -5 build.log",
            "sleep 29; sleep 29; tail log",
            "sleep 1m; ls",
            "until grep -q DONE log; do sleep 30; done",
            "while true; do sleep 5; done",
            "cd repo && gh run watch 123",
            "gh pr checks 7 --watch",
            "/bin/sleep 600 && cat log",
        ] {
            assert!(rejection(script).is_some(), "{script}");
        }
    }

    #[test]
    fn allows_ordinary_commands() {
        for script in [
            "cargo test",
            "sleep 2; curl -s localhost:8765",
            "until nc -z 127.0.0.1 3000; do sleep 0.1; done",
            "gh run list --limit 4",
            "gh pr checks 7",
            "printf ready; sleep 30",
            "sleep 30 & printf '%s' $!",
            "echo sleepy; grep -r sleep src",
        ] {
            assert!(rejection(script).is_none(), "{script}");
        }
    }
}
