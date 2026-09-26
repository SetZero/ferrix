//! `ferrix-statd`: Ferrix's stat service.
//!
//! A program like any other on Ferrix, reading what Linux's `vmstat` and
//! `top` read: `/proc/stat`, `/proc/meminfo`, `/proc/uptime` and each
//! process's `/proc/<pid>/stat`. Every interval it writes one line to its
//! standard output, which as pid 1 is the console:
//!
//! ```text
//! FERRIX-STAT {"t":12.34,"cpu":[3.1,0.0,...],"mem":{...},"tasks":{...},"top":[...]}
//! ```
//!
//! The console is a crosvm guest's 16550, or on the phone the `ramoops`
//! record Android reads back after the run, so `tools/pixel7-monitor` graphs
//! a guest live and a native boot afterwards. Ferrix's boot console draws only
//! the kernel's own lines, so the screen is not filled with these.
//!
//! Started as pid 1 with `ferrix.init=/sbin/ferrix-statd`, after the boot
//! checks. It reads its settings from `/proc/cmdline`:
//!
//! * `ferrix.statd.interval=<ms>`: between samples, 500 by default, 100 at
//!   least.
//! * `ferrix.statd.seconds=<n>`: how long to run, 0 (the default) for until
//!   the machine is stopped. When it ends, pid 1 exits and the kernel powers
//!   off, which on the phone is the watchdog's reset back to Android.
//!
//! One `FERRIX-STAT-START` line comes first, with what does not change, and
//! one `FERRIX-STAT-END` line last. Everything is JSON the program writes
//! itself: it has no dependencies.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::thread;
use std::time::{Duration, Instant};

/// Clock ticks a second in `/proc/<pid>/stat`, Linux's `USER_HZ`.
const USER_HZ: f64 = 100.0;

/// How many of the busiest processes each sample names.
const TOP: usize = 5;

/// One processor's times from `/proc/stat`, in ticks: busy, and idle.
#[derive(Clone, Copy, Default)]
struct Times {
    busy: u64,
    idle: u64,
}

/// What `/proc/stat` says, beyond each processor's times.
#[derive(Default)]
struct Kstat {
    total: Times,
    cpus: Vec<Times>,
    interrupts: u64,
    switches: u64,
    forks: u64,
    running: u64,
    blocked: u64,
}

/// One process, from its `stat`.
struct Task {
    pid: u32,
    comm: String,
    state: char,
    ticks: u64,
    threads: u64,
    rss_pages: u64,
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// The value of `name=` on the kernel command line.
fn option<'a>(cmdline: &'a str, name: &str) -> Option<&'a str> {
    cmdline
        .split_whitespace()
        .find_map(|word| word.strip_prefix(name)?.strip_prefix('='))
}

fn times(fields: &[&str]) -> Times {
    let numbers: Vec<u64> = fields.iter().filter_map(|x| x.parse().ok()).collect();
    let at = |i: usize| numbers.get(i).copied().unwrap_or(0);
    let idle = at(3) + at(4);
    let all: u64 = numbers.iter().take(8).sum();
    Times {
        busy: all - idle,
        idle,
    }
}

fn kstat() -> Kstat {
    let mut stat = Kstat::default();
    for line in read("/proc/stat").lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some((&name, rest)) = fields.split_first() else {
            continue;
        };
        let first = || rest.first().and_then(|x| x.parse().ok()).unwrap_or(0);
        match name {
            "cpu" => stat.total = times(rest),
            _ if name.starts_with("cpu") => stat.cpus.push(times(rest)),
            "intr" => stat.interrupts = first(),
            "ctxt" => stat.switches = first(),
            "processes" => stat.forks = first(),
            "procs_running" => stat.running = first(),
            "procs_blocked" => stat.blocked = first(),
            _ => {}
        }
    }
    stat
}

/// `/proc/meminfo`, in KiB, by name.
fn meminfo() -> HashMap<String, u64> {
    read("/proc/meminfo")
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let kib = rest.split_whitespace().next()?.parse().ok()?;
            Some((name.trim().to_string(), kib))
        })
        .collect()
}

fn uptime() -> f64 {
    read("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|x| x.parse().ok())
        .unwrap_or(0.0)
}

/// Every process `/proc` lists.
fn tasks() -> Vec<Task> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let text = read(&format!("/proc/{pid}/stat"));
        // `pid (comm) state ...`: the name may hold spaces and parentheses,
        // so it runs to the last ')'.
        let (Some(open), Some(close)) = (text.find('('), text.rfind(')')) else {
            continue;
        };
        let comm = text[open + 1..close].to_string();
        let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
        // Fields after the name, counted from `state` as 0: utime is 11,
        // stime 12, num_threads 17, rss 21.
        let at = |i: usize| rest.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        tasks.push(Task {
            pid,
            comm,
            state: rest.first().and_then(|s| s.chars().next()).unwrap_or('?'),
            ticks: at(11) + at(12),
            threads: at(17),
            rss_pages: at(21),
        });
    }
    tasks
}

fn load(now: Times, then: Times) -> f64 {
    let busy = now.busy.saturating_sub(then.busy) as f64;
    let idle = now.idle.saturating_sub(then.idle) as f64;
    if busy + idle > 0.0 {
        100.0 * busy / (busy + idle)
    } else {
        0.0
    }
}

/// `text` as a JSON string.
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn main() {
    let cmdline = read("/proc/cmdline");
    let interval_ms: u64 = option(&cmdline, "ferrix.statd.interval")
        .and_then(|x| x.parse().ok())
        .unwrap_or(500)
        .max(100);
    let seconds: u64 = option(&cmdline, "ferrix.statd.seconds")
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    let page_kib = 4;

    let mut then = kstat();
    let mut then_tasks: HashMap<u32, u64> = tasks().into_iter().map(|t| (t.pid, t.ticks)).collect();
    let mut then_at = Instant::now();
    let memory = meminfo();
    say(&format!(
        "FERRIX-STAT-START {{\"version\":1,\"interval_ms\":{interval_ms},\"seconds\":{seconds},\"cpus\":{},\"mem_total_kib\":{},\"pid\":{}}}",
        then.cpus.len(),
        memory.get("MemTotal").copied().unwrap_or(0),
        std::process::id(),
    ));

    let began = Instant::now();
    let mut samples = 0u64;
    loop {
        thread::sleep(Duration::from_millis(interval_ms));
        let now = kstat();
        let now_tasks = tasks();
        let now_at = Instant::now();
        let elapsed = now_at.duration_since(then_at).as_secs_f64().max(0.001);
        let memory = meminfo();
        let kib = |name: &str| memory.get(name).copied().unwrap_or(0);

        let mut line = String::from("FERRIX-STAT {");
        let _ = write!(line, "\"t\":{:.2}", uptime());
        let _ = write!(line, ",\"all\":{:.1}", load(now.total, then.total));
        line.push_str(",\"cpu\":[");
        for (i, cpu) in now.cpus.iter().enumerate() {
            let was = then.cpus.get(i).copied().unwrap_or_default();
            let _ = write!(
                line,
                "{}{:.1}",
                if i > 0 { "," } else { "" },
                load(*cpu, was)
            );
        }
        let _ = write!(
            line,
            "],\"mem\":{{\"total\":{},\"free\":{},\"available\":{},\"cached\":{},\"buffers\":{}}}",
            kib("MemTotal"),
            kib("MemFree"),
            kib("MemAvailable"),
            kib("Cached"),
            kib("Buffers"),
        );
        let per_second = |now: u64, was: u64| now.saturating_sub(was) as f64 / elapsed;
        let _ = write!(
            line,
            ",\"rate\":{{\"irq\":{:.0},\"ctxt\":{:.0},\"forks\":{:.1}}}",
            per_second(now.interrupts, then.interrupts),
            per_second(now.switches, then.switches),
            per_second(now.forks, then.forks),
        );

        let mut states: HashMap<char, u64> = HashMap::new();
        for task in &now_tasks {
            *states.entry(task.state).or_default() += 1;
        }
        let threads: u64 = now_tasks.iter().map(|t| t.threads.max(1)).sum();
        let _ = write!(
            line,
            ",\"tasks\":{{\"processes\":{},\"threads\":{},\"running\":{},\"blocked\":{},\"sleeping\":{}}}",
            now_tasks.len(),
            threads,
            now.running,
            now.blocked,
            states.get(&'S').copied().unwrap_or(0) + states.get(&'D').copied().unwrap_or(0),
        );

        // The busiest processes since the last sample, as a share of one
        // processor.
        let mut busiest: Vec<(f64, &Task)> = now_tasks
            .iter()
            .map(|task| {
                let was = then_tasks.get(&task.pid).copied().unwrap_or(task.ticks);
                (
                    task.ticks.saturating_sub(was) as f64 / USER_HZ / elapsed * 100.0,
                    task,
                )
            })
            .collect();
        busiest.sort_by(|a, b| b.0.total_cmp(&a.0));
        line.push_str(",\"top\":[");
        for (i, (cpu, task)) in busiest.iter().take(TOP).enumerate() {
            let _ = write!(
                line,
                "{}{{\"pid\":{},\"comm\":{},\"cpu\":{:.1},\"rss_kib\":{},\"threads\":{}}}",
                if i > 0 { "," } else { "" },
                task.pid,
                quoted(&task.comm),
                cpu,
                task.rss_pages * page_kib,
                task.threads,
            );
        }
        line.push_str("]}");
        say(&line);

        samples += 1;
        then = now;
        then_tasks = now_tasks.iter().map(|t| (t.pid, t.ticks)).collect();
        then_at = now_at;
        if seconds > 0 && began.elapsed().as_secs() >= seconds {
            break;
        }
    }
    say(&format!(
        "FERRIX-STAT-END {{\"samples\":{samples},\"seconds\":{:.1}}}",
        began.elapsed().as_secs_f64()
    ));
}
