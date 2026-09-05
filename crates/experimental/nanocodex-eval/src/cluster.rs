use std::{ffi::OsStr, fs, time::SystemTime};

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub(crate) struct HostSampler {
    system: System,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClusterSnapshot {
    schema_version: u32,
    observed_at_ms: u64,
    nodes: Vec<NodeSnapshot>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeSnapshot {
    id: String,
    observed_at_ms: u64,
    uptime_seconds: u64,
    claimed_tasks: usize,
    worker_processes: usize,
    vm_processes: usize,
    cpu_cores: usize,
    cpu_usage_percent: f32,
    load_average: LoadAverage,
    memory: Capacity,
    swap: Capacity,
    pressure: Pressure,
}

#[derive(Debug, Serialize)]
struct LoadAverage {
    one: f64,
    five: f64,
    fifteen: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Capacity {
    total_bytes: u64,
    available_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Pressure {
    cpu_some_avg10: Option<f32>,
    memory_some_avg10: Option<f32>,
    memory_full_avg10: Option<f32>,
}

impl HostSampler {
    pub(crate) fn new() -> Self {
        Self {
            system: System::new_all(),
        }
    }

    pub(crate) fn snapshot(&mut self, claimed_tasks: usize) -> ClusterSnapshot {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.refresh_processes();

        let observed_at_ms = SystemTime::UNIX_EPOCH
            .elapsed()
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let cpu_cores = self.system.cpus().len();
        let load = System::load_average();
        let pressure = read_pressure();
        let memory = Capacity {
            total_bytes: self.system.total_memory(),
            available_bytes: self.system.available_memory(),
        };
        let swap = Capacity {
            total_bytes: self.system.total_swap(),
            available_bytes: self.system.free_swap(),
        };
        let (worker_processes, vm_processes) = process_counts(&self.system);
        let node = NodeSnapshot {
            id: System::host_name().unwrap_or_else(|| "unknown-host".to_owned()),
            observed_at_ms,
            uptime_seconds: System::uptime(),
            claimed_tasks,
            worker_processes,
            vm_processes,
            cpu_cores,
            cpu_usage_percent: self.system.global_cpu_usage(),
            load_average: LoadAverage {
                one: load.one,
                five: load.five,
                fifteen: load.fifteen,
            },
            memory,
            swap,
            pressure,
        };
        ClusterSnapshot {
            schema_version: 1,
            observed_at_ms,
            nodes: vec![node],
        }
    }

    pub(crate) fn worker_is_live(&mut self, worker: &str) -> bool {
        self.refresh_processes();
        self.system.processes().values().any(|process| {
            is_eval_worker(process.name(), process.cmd())
                && has_argument_value(process.cmd(), "--worker", worker)
        })
    }

    fn refresh_processes(&mut self) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .without_tasks(),
        );
    }
}

fn process_counts(system: &System) -> (usize, usize) {
    let mut workers = 0;
    let mut vms = 0;
    for process in system.processes().values() {
        let arguments = process.cmd();
        if is_eval_worker(process.name(), arguments) {
            workers += 1;
        }
        // libkrun changes the process name to `libkrun VM` after startup, but
        // Linux retains the original nanocodex command line.
        if arguments
            .iter()
            .any(|argument| argument == OsStr::new("vm-run-config"))
        {
            vms += 1;
        }
    }
    (workers, vms)
}

fn is_eval_worker(name: &OsStr, arguments: &[std::ffi::OsString]) -> bool {
    name == OsStr::new("nanocodex") && has_argument_value(arguments, "eval", "run")
}

fn has_argument_value(arguments: &[std::ffi::OsString], argument: &str, value: &str) -> bool {
    arguments
        .windows(2)
        .any(|pair| pair[0] == OsStr::new(argument) && pair[1] == OsStr::new(value))
}

fn read_pressure() -> Pressure {
    Pressure {
        cpu_some_avg10: read_psi("/proc/pressure/cpu", "some"),
        memory_some_avg10: read_psi("/proc/pressure/memory", "some"),
        memory_full_avg10: read_psi("/proc/pressure/memory", "full"),
    }
}

fn read_psi(path: &str, class: &str) -> Option<f32> {
    let contents = fs::read_to_string(path).ok()?;
    parse_psi(&contents, class)
}

fn parse_psi(contents: &str, class: &str) -> Option<f32> {
    contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != class {
            return None;
        }
        fields.find_map(|field| {
            field
                .strip_prefix("avg10=")
                .and_then(|value| value.parse().ok())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pressure_stall_avg10_by_class() {
        let pressure = "some avg10=12.06 avg60=24.93 avg300=29.21 total=7\n\
                        full avg10=11.88 avg60=24.45 avg300=28.80 total=5\n";
        assert_eq!(parse_psi(pressure, "some"), Some(12.06));
        assert_eq!(parse_psi(pressure, "full"), Some(11.88));
    }
}
