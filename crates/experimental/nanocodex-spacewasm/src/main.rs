use std::io::{self, BufRead, Write};

use nanocodex_spacewasm::{Command, FlightCore, HostAction};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut core = FlightCore::new();

    for line in stdin.lock().lines() {
        let action = match line {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => match serde_json::from_str::<Command>(&line) {
                Ok(command) => core.apply(command),
                Err(error) => {
                    let _ = writeln!(
                        io::stderr(),
                        "nanocodex-spacewasm: invalid command: {error}"
                    );
                    continue;
                }
            },
            Err(error) => {
                let _ = writeln!(io::stderr(), "nanocodex-spacewasm: stdin failed: {error}");
                break;
            }
        };
        let shutdown = matches!(action, HostAction::Shutdown { .. });
        if serde_json::to_writer(&mut stdout, &action).is_err()
            || writeln!(&mut stdout).is_err()
            || stdout.flush().is_err()
        {
            break;
        }
        if shutdown {
            break;
        }
    }
}
