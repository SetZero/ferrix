//! The phone, polled once a second: what state it is in, and while Android
//! runs, its processors, memory, temperatures, GPU, battery and any crosvm.
//!
//! The stats come from one shell script per tick, fed to a shell that stays
//! open. It is a root shell where Magisk allows one, since the thermal zones,
//! the GPU and the memory bus are root's to read, and a plain shell otherwise,
//! which still reads the processors, memory and battery. One `su` for the
//! whole session, rather than one a second, which Magisk would announce each
//! time.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::helper;

/// The phone, by its serial number.
pub fn serial() -> String {
    std::env::var("PIXEL7_SERIAL").unwrap_or_else(|_| "28171FDH2001RC".into())
}

/// Where each tick's output ends.
const END: &str = "__PIXEL7_MONITOR_END__";

/// The script each tick runs on the phone. Each section starts with a line
/// `@name`, and whatever the shell cannot read is left out rather than
/// reported.
const SCRIPT: &str = r#"echo @stat; grep '^cpu' /proc/stat
echo @mem; grep -E '^(MemTotal|MemAvailable|Cached|SwapTotal|SwapFree):' /proc/meminfo
echo @freq; for p in /sys/devices/system/cpu/cpufreq/policy*; do echo "$(cut -d' ' -f1 $p/related_cpus) $(cat $p/scaling_cur_freq) $(cat $p/cpuinfo_max_freq)"; done
echo @thermal; for z in /sys/class/thermal/thermal_zone*; do t=$(cat $z/temp 2>/dev/null) && echo "$(cat $z/type) $t"; done 2>/dev/null
echo @gpu; cat /sys/devices/platform/28000000.mali/cur_freq /sys/devices/platform/28000000.mali/utilization 2>/dev/null
echo @mif; cat /sys/class/devfreq/17000010.devfreq_mif/cur_freq 2>/dev/null
echo @bat; for f in capacity status current_now voltage_now temp; do echo "$f $(cat /sys/class/power_supply/battery/$f 2>/dev/null)"; done
echo @load; cat /proc/loadavg
echo @vm; for x in $(pidof crosvm); do echo "$x $(cut -d' ' -f14,15 /proc/$x/stat) $(cut -d' ' -f2 /proc/$x/statm)"; done
echo @id; id -u
"#;

/// One processor cluster: its first processor, and its frequency now and at
/// most, in MHz.
#[derive(Clone, Debug, Serialize)]
pub struct Cluster {
    first_cpu: u32,
    mhz: f64,
    max_mhz: f64,
}

/// Memory, in MiB.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Memory {
    total: f64,
    available: f64,
    cached: f64,
    swap_total: f64,
    swap_free: f64,
}

/// The battery.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Battery {
    level: Option<f64>,
    status: Option<String>,
    current_ma: Option<f64>,
    voltage: Option<f64>,
    temp_c: Option<f64>,
}

/// A crosvm process: its CPU use since the last tick, in percent of one
/// processor, and its resident memory in MiB.
#[derive(Clone, Debug, Serialize)]
pub struct Vm {
    pid: u32,
    cpu: f64,
    rss_mib: f64,
}

/// What the phone was doing at one tick.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Sample {
    /// Seconds since the Unix epoch.
    t: f64,
    /// `android`, `fastboot`, `ferrix` (gone from USB after a `fastboot
    /// boot`), `unauthorized`, `offline` or `absent`.
    state: String,
    /// Whether the stats came from a root shell.
    root: bool,
    /// Each processor's load since the last tick, in percent.
    cores: Vec<f64>,
    /// All of them together.
    total: Option<f64>,
    clusters: Vec<Cluster>,
    memory: Option<Memory>,
    /// Every thermal zone the shell could read, in °C.
    thermal: Vec<(String, f64)>,
    /// The GPU's frequency in MHz and its load in percent.
    gpu: Option<(f64, f64)>,
    /// The memory interface's frequency, in MHz.
    mif_mhz: Option<f64>,
    battery: Option<Battery>,
    load: Option<[f64; 3]>,
    vms: Vec<Vm>,
    /// What the launcher's helper says, if it is running.
    helper: Option<serde_json::Value>,
}

/// A shell on the phone that stays open, and reads what it prints.
struct Shell {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    root: bool,
}

impl Shell {
    /// A root shell, or failing that a plain one.
    fn open(serial: &str) -> Option<Shell> {
        Shell::start(serial, &["shell", "su"]).or_else(|| Shell::start(serial, &["shell"]))
    }

    fn start(serial: &str, arguments: &[&str]) -> Option<Shell> {
        let mut child = Command::new("adb")
            .arg("-s")
            .arg(serial)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut shell = Shell { child, stdin, lines, root: false };
        // Ask who it is: a `su` Magisk refused has already exited.
        let answer = shell.run("id -u\n", Duration::from_secs(20))?;
        shell.root = answer.first().map(|id| id.trim() == "0").unwrap_or(false);
        Some(shell)
    }

    /// Run `script` and collect what it printed, or `None` if the shell died
    /// or took longer than `patience`.
    fn run(&mut self, script: &str, patience: Duration) -> Option<Vec<String>> {
        self.stdin.write_all(script.as_bytes()).ok()?;
        self.stdin.write_all(format!("echo {END}\n").as_bytes()).ok()?;
        self.stdin.flush().ok()?;
        let deadline = Instant::now() + patience;
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if line.trim() == END => return Some(out),
                Ok(line) => out.push(line),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where the phone is: in `adb`, in `fastboot`, or neither.
fn usb_state(serial: &str) -> String {
    let adb = Command::new("adb").arg("devices").output();
    if let Ok(adb) = adb {
        for line in String::from_utf8_lossy(&adb.stdout).lines() {
            let mut fields = line.split_whitespace();
            if fields.next() == Some(serial) {
                return match fields.next() {
                    Some("device") => "android",
                    Some("unauthorized") => "unauthorized",
                    Some("recovery") => "recovery",
                    _ => "offline",
                }
                .into();
            }
        }
    }
    let fastboot = Command::new("fastboot").arg("devices").output();
    if let Ok(fastboot) = fastboot
        && String::from_utf8_lossy(&fastboot.stdout).contains(serial)
    {
        return "fastboot".into();
    }
    "absent".into()
}

/// Poll the phone for as long as the app runs, and send each sample to the
/// window as the event `sample`.
pub fn poll(app: AppHandle) {
    let serial = serial();
    let mut shell: Option<Shell> = None;
    let mut previous_cpu: Vec<(u64, u64)> = Vec::new();
    let mut previous_vm: Vec<(u32, u64, Instant)> = Vec::new();
    let mut last_usb = String::new();
    loop {
        let began = Instant::now();
        let usb = usb_state(&serial);
        let helper = helper::status();
        let helper_running_ferrix = helper
            .as_ref()
            .and_then(|status| status.get("phase"))
            .and_then(|phase| phase.as_str())
            .is_some_and(|phase| phase.starts_with("Ferrix"));
        // Gone from USB straight after fastboot, or while the helper says
        // Ferrix is running: the phone is in Ferrix, which has no USB.
        let state = if usb == "absent" && (last_usb == "fastboot" || last_usb == "ferrix" || helper_running_ferrix) {
            "ferrix".to_string()
        } else {
            usb.clone()
        };
        last_usb = state.clone();

        let mut sample = Sample {
            t: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            state: state.clone(),
            helper,
            ..Sample::default()
        };
        if state == "android" {
            if shell.is_none() {
                shell = Shell::open(&serial);
            }
            match shell.as_mut().and_then(|shell| shell.run(SCRIPT, Duration::from_secs(5))) {
                Some(lines) => {
                    sample.root = shell.as_ref().is_some_and(|shell| shell.root);
                    parse(&lines, &mut sample, &mut previous_cpu, &mut previous_vm);
                }
                None => shell = None,
            }
        } else {
            shell = None;
            previous_cpu.clear();
            previous_vm.clear();
        }
        let _ = app.emit("sample", &sample);
        thread::sleep(Duration::from_secs(1).saturating_sub(began.elapsed()));
    }
}

/// Fill `sample` from one tick's output.
fn parse(
    lines: &[String],
    sample: &mut Sample,
    previous_cpu: &mut Vec<(u64, u64)>,
    previous_vm: &mut Vec<(u32, u64, Instant)>,
) {
    let mut section = "";
    let mut cpu_now: Vec<(u64, u64)> = Vec::new();
    let mut memory = Memory::default();
    let mut battery = Battery::default();
    let mut gpu = Vec::new();
    let mut vms = Vec::new();
    for line in lines {
        if let Some(name) = line.strip_prefix('@') {
            section = match name.trim() {
                "stat" => "stat",
                "mem" => "mem",
                "freq" => "freq",
                "thermal" => "thermal",
                "gpu" => "gpu",
                "mif" => "mif",
                "bat" => "bat",
                "load" => "load",
                "vm" => "vm",
                _ => "",
            };
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        match section {
            // `cpu` then `cpuN`: user nice system idle iowait irq softirq steal.
            "stat" if fields.len() > 5 => {
                let numbers: Vec<u64> = fields[1..].iter().filter_map(|x| x.parse().ok()).collect();
                let idle = numbers.get(3).copied().unwrap_or(0) + numbers.get(4).copied().unwrap_or(0);
                let busy: u64 = numbers.iter().take(8).sum::<u64>() - idle;
                cpu_now.push((busy, idle));
            }
            "mem" if fields.len() >= 2 => {
                let mib = fields[1].parse::<f64>().unwrap_or(0.0) / 1024.0;
                match fields[0] {
                    "MemTotal:" => memory.total = mib,
                    "MemAvailable:" => memory.available = mib,
                    "Cached:" => memory.cached = mib,
                    "SwapTotal:" => memory.swap_total = mib,
                    "SwapFree:" => memory.swap_free = mib,
                    _ => {}
                }
            }
            "freq" if fields.len() >= 3 => {
                if let (Ok(first_cpu), Ok(cur), Ok(max)) =
                    (fields[0].parse(), fields[1].parse::<f64>(), fields[2].parse::<f64>())
                {
                    sample.clusters.push(Cluster { first_cpu, mhz: cur / 1000.0, max_mhz: max / 1000.0 });
                }
            }
            "thermal" if fields.len() >= 2 => {
                if let Ok(milli) = fields[1].parse::<f64>() {
                    sample.thermal.push((fields[0].to_string(), milli / 1000.0));
                }
            }
            "gpu" => gpu.extend(fields.iter().filter_map(|x| x.parse::<f64>().ok())),
            "mif" => sample.mif_mhz = fields.first().and_then(|x| x.parse::<f64>().ok()).map(|k| k / 1000.0),
            "bat" if !fields.is_empty() => {
                let value = fields.get(1).copied();
                let number = value.and_then(|x| x.parse::<f64>().ok());
                match fields[0] {
                    "capacity" => battery.level = number,
                    "status" => battery.status = (fields.len() > 1).then(|| fields[1..].join(" ")),
                    "current_now" => battery.current_ma = number.map(|ua| ua / 1000.0),
                    "voltage_now" => battery.voltage = number.map(|uv| uv / 1_000_000.0),
                    "temp" => battery.temp_c = number.map(|t| t / 10.0),
                    _ => {}
                }
            }
            "load" if fields.len() >= 3 => {
                let n = |i: usize| fields[i].parse::<f64>().unwrap_or(0.0);
                sample.load = Some([n(0), n(1), n(2)]);
            }
            "vm" if fields.len() >= 4 => {
                if let (Ok(pid), Ok(user), Ok(system), Ok(pages)) = (
                    fields[0].parse::<u32>(),
                    fields[1].parse::<u64>(),
                    fields[2].parse::<u64>(),
                    fields[3].parse::<f64>(),
                ) {
                    vms.push((pid, user + system, pages * 4.0 / 1024.0));
                }
            }
            _ => {}
        }
    }

    // Processor load from the change since the last tick: the first line is
    // every processor together, the rest one each.
    if cpu_now.len() == previous_cpu.len() {
        let loads: Vec<f64> = cpu_now
            .iter()
            .zip(previous_cpu.iter())
            .map(|(&(busy, idle), &(was_busy, was_idle))| {
                let busy = busy.saturating_sub(was_busy) as f64;
                let idle = idle.saturating_sub(was_idle) as f64;
                if busy + idle > 0.0 { 100.0 * busy / (busy + idle) } else { 0.0 }
            })
            .collect();
        if let Some((total, cores)) = loads.split_first() {
            sample.total = Some(*total);
            sample.cores = cores.to_vec();
        }
    }
    *previous_cpu = cpu_now;

    if memory.total > 0.0 {
        sample.memory = Some(memory);
    }
    if battery.level.is_some() {
        sample.battery = Some(battery);
    }
    if let [freq, util, ..] = gpu[..] {
        sample.gpu = Some((freq / 1000.0, util));
    }

    // crosvm's processor time since the last tick, at 100 ticks a second.
    let now = Instant::now();
    sample.vms = vms
        .iter()
        .map(|&(pid, ticks, rss_mib)| {
            let cpu = previous_vm
                .iter()
                .find(|(was_pid, _, _)| *was_pid == pid)
                .map(|&(_, was_ticks, then)| {
                    let seconds = now.duration_since(then).as_secs_f64().max(0.001);
                    ticks.saturating_sub(was_ticks) as f64 / seconds
                })
                .unwrap_or(0.0);
            Vm { pid, cpu, rss_mib }
        })
        .collect();
    *previous_vm = vms.iter().map(|&(pid, ticks, _)| (pid, ticks, now)).collect();
}
