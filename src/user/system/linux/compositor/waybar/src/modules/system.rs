//! `cpu` and `memory`: numbers from `/proc`, as waybar reads them.
//!
//! **cpu** (`modules/cpu.cpp`, `cpu_usage/linux.cpp`, `load.cpp`): each
//! `cpu` line of `/proc/stat` is idle (idle + iowait) against the sum of
//! every field, and usage is `100 × (1 − Δidle / Δtotal)` since the last
//! read -- the first read waits 100 ms for a second sample, as waybar
//! does. `{load}` is the one-minute load average from `getloadavg(3)`,
//! rounded up to hundredths. Ferrix has no `/proc/loadavg` and its
//! `sysinfo(2)` reports loads of zero, so there `{load}` is `0` and the bar
//! says once why; a C library's `getloadavg` would answer the same zeros
//! (musl's reads `sysinfo`) or fail (glibc's reads `/proc/loadavg`).
//! `{max_frequency}`, `{min_frequency}` and `{avg_frequency}` come from
//! `/proc/cpuinfo`'s `cpu MHz`, in GHz.
//!
//! **memory** (`memory/common.cpp`, `memory/linux.cpp`): `/proc/meminfo` in
//! kiB, available memory being `MemAvailable` where the kernel has it;
//! `{percentage}` is used over total, and `{total}`, `{used}`, `{avail}` and
//! the swap ones are in `unit` (GiB unless it says), each rounded to
//! hundredths and printed as a `float`.

use std::time::Duration;

use super::{Common, Host, Module, TimerKey};
use crate::fmt::{Arg, Args, format};
use crate::json::Value;
use crate::view::{ModuleView, Shape};

/// One `cpu` line: idle and total ticks.
pub type Times = (u64, u64);

/// `CpuUsage::parseCpuinfo`: the total line and then one per processor,
/// padded to `/sys/devices/system/cpu/present`'s last with zeros for ones
/// that are offline.
#[must_use]
pub fn parse_stat(text: &str, present: Option<&str>) -> Vec<Times> {
    let last_present = present
        .and_then(|line| line.lines().next())
        .and_then(|line| line.rsplit(['-', ',']).next())
        .and_then(|last| last.trim().parse::<usize>().ok());
    let mut out: Vec<Times> = Vec::new();
    // The first line is the total, then processor 0.
    let mut current: Option<usize> = None;
    for line in text.lines() {
        if !line.starts_with("cpu") {
            break;
        }
        let mut fields = line.split_whitespace();
        let label = fields.next().unwrap_or_default();
        if let Some(number) = label
            .strip_prefix("cpu")
            .and_then(|n| n.parse::<usize>().ok())
        {
            let mut next = current.map_or(0, |c| c + 1);
            while number > next {
                out.push((0, 0));
                next += 1;
            }
            current = Some(number);
        } else {
            current = None;
        }
        let times: Vec<u64> = fields.map_while(|f| f.parse().ok()).collect();
        let (idle, total) = match times.as_slice() {
            [_, _, _, idle, iowait, ..] => (idle + iowait, times.iter().sum()),
            _ => (0, 0),
        };
        out.push((idle, total));
    }
    if let Some(last) = last_present {
        // One line per processor after the total.
        while out.len() < last + 2 {
            out.push((0, 0));
        }
    }
    out
}

/// `CpuUsage::getCpuUsage`: usage per line since `before`, and the default
/// tooltip.
#[must_use]
pub fn usage(before: &[Times], now: &[Times]) -> (Vec<u16>, String) {
    let percent = |(idle0, total0): Times, (idle1, total1): Times| -> u16 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "tick counts, as waybar's floats"
        )]
        let (d_idle, d_total) = (
            idle1.saturating_sub(idle0) as f32,
            total1.saturating_sub(total0) as f32,
        );
        if d_total > 0.0 {
            #[expect(clippy::cast_possible_truncation, reason = "a percentage")]
            #[expect(clippy::cast_sign_loss, reason = "a percentage")]
            let value = (100.0 * (1.0 - d_idle / d_total)) as u16;
            value
        } else {
            0
        }
    };
    if before.len() != now.len() {
        return match (before.first(), now.first()) {
            (Some(&a), Some(&b)) => {
                let total = percent(a, b);
                (vec![total], format!("Total: {total}%\nCores: (pending)"))
            }
            _ => (vec![0], "(pending)".to_owned()),
        };
    }
    let mut tooltip = String::new();
    let mut out = Vec::new();
    for (index, (&a, &b)) in before.iter().zip(now).enumerate() {
        if index > 0 && (a.1 == 0 || b.1 == 0) {
            tooltip.push_str(&format!("\nCore{}: offline", index - 1));
            out.push(0);
            continue;
        }
        let value = percent(a, b);
        if index == 0 {
            tooltip = format!("Total: {value}%");
        } else {
            tooltip.push_str(&format!("\nCore{}: {value}%", index - 1));
        }
        out.push(value);
    }
    (out, tooltip)
}

/// The load averages, `getloadavg`'s three, rounded up to hundredths; and
/// whether they are real.
#[must_use]
pub fn load(loadavg: Option<&str>) -> ([f64; 3], bool) {
    let Some(text) = loadavg else {
        return ([0.0; 3], false);
    };
    let mut values = text
        .split_whitespace()
        .map(|v| v.parse::<f64>().unwrap_or(0.0));
    let mut next = || ((values.next().unwrap_or(0.0)) * 100.0).ceil() / 100.0;
    ([next(), next(), next()], true)
}

/// `/proc/cpuinfo`'s `cpu MHz` lines: the largest, smallest and mean, in GHz.
#[must_use]
pub fn frequencies(cpuinfo: Option<&str>) -> (f32, f32, f32) {
    let list: Vec<f32> = cpuinfo
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("cpu MHz"))
        .filter_map(|line| line.split_once(':'))
        .filter_map(|(_, value)| value.trim().split('.').next()?.parse::<f32>().ok())
        .collect();
    if list.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let max = list.iter().copied().fold(f32::MIN, f32::max);
    let min = list.iter().copied().fold(f32::MAX, f32::min);
    #[expect(clippy::cast_precision_loss, reason = "a count of processors")]
    let mean = list.iter().sum::<f32>() / list.len() as f32;
    let ghz = |mhz: f32| (mhz / 10.0).round() / 100.0;
    (ghz(max), ghz(min), ghz(mean))
}

/// The `cpu` module.
#[derive(Debug)]
pub struct Cpu {
    common: Common,
    before: Vec<Times>,
    timer: Option<TimerKey>,
    /// Waiting the 100 ms for a first second sample.
    priming: bool,
    view: ModuleView,
}

impl Cpu {
    /// Make it.
    #[must_use]
    pub fn new(name: &str, config: &Value) -> Self {
        let common = Common::new(name, config, "{usage}%", 10);
        let view = common.view("cpu", Shape::Label);
        Self {
            common,
            before: Vec::new(),
            timer: None,
            priming: false,
            view,
        }
    }

    fn sample(host: &mut dyn Host) -> Vec<Times> {
        let stat = host.read("/proc/stat").unwrap_or_default();
        let present = host.read("/sys/devices/system/cpu/present");
        parse_stat(&stat, present.as_deref())
    }

    fn update(&mut self, host: &mut dyn Host) {
        let now = Self::sample(host);
        let (usages, tooltip) = usage(&self.before, &now);
        self.before = now;
        let loadavg = host.read("/proc/loadavg");
        let (loads, real) = load(loadavg.as_deref());
        if !real {
            host.diag().once(
                "cpu-load",
                "cpu: no /proc/loadavg: Ferrix does not keep load averages, so {load} is 0"
                    .to_owned(),
            );
        }
        let cpuinfo = host.read("/proc/cpuinfo");
        let (max, min, avg) = frequencies(cpuinfo.as_deref());
        let total = usages.first().copied().unwrap_or(0);
        let (state, states) = self
            .common
            .state(u8::try_from(total.min(255)).unwrap_or(255), false);
        let mut label_format = self.common.format.clone();
        if !state.is_empty()
            && let Some(state_format) = self.common.config.get(&format!("format-{state}")).as_str()
        {
            label_format = state_format.to_owned();
        }
        if label_format.is_empty() {
            self.view.visible = false;
            return;
        }
        self.view.visible = true;
        let [load1, load5, load15] = loads;
        let mut args = Args::new()
            .named("load", Arg::F64(load1))
            .named("load1", Arg::F64(load1))
            .named("load5", Arg::F64(load5))
            .named("load15", Arg::F64(load15))
            .named("usage", u32::from(total))
            .named("icon", self.common.icon(total, &[&state], 0))
            .named("max_frequency", Arg::F32(max))
            .named("min_frequency", Arg::F32(min))
            .named("avg_frequency", Arg::F32(avg));
        let mut icons = String::new();
        for (core, &value) in usages.iter().skip(1).enumerate() {
            args.push(&format!("usage{core}"), u32::from(value));
            let icon = self.common.icon(value, &[&state], 0);
            icons.push_str(&icon);
            args.push(&format!("icon{core}"), icon);
        }
        args.push("icons", icons);
        set_label(
            &mut self.view,
            &self.common,
            host,
            &label_format,
            &tooltip,
            &state,
            &args,
        );
        self.view.classes.retain(|class| !states.contains(class));
        if !state.is_empty() {
            self.view.classes.push(state);
        }
    }
}

/// `updateLabelAndTooltipForState`: the label and the tooltip from one set
/// of arguments.
pub fn set_label(
    view: &mut ModuleView,
    common: &Common,
    host: &mut dyn Host,
    label_format: &str,
    default_tooltip: &str,
    state: &str,
    args: &Args,
) {
    match format(label_format, args) {
        Ok(text) => view.markup = text,
        Err(error) => host.diag().error(format!("{}: {error}", common.name)),
    }
    if common.tooltip {
        match format(&common.tooltip_format(default_tooltip, state), args) {
            Ok(text) => view.tooltip = Some(text),
            Err(error) => host.diag().error(format!("{}: {error}", common.name)),
        }
    } else {
        view.tooltip = None;
    }
}

impl Module for Cpu {
    fn start(&mut self, host: &mut dyn Host) {
        if self.before.is_empty() {
            // The first reading needs one before it, 100 ms earlier.
            self.before = Self::sample(host);
            self.priming = true;
            self.timer = Some(host.timer(Duration::from_millis(100)));
            return;
        }
        self.update(host);
    }

    fn view(&self) -> &ModuleView {
        &self.view
    }

    fn common(&mut self) -> &mut Common {
        &mut self.common
    }

    fn timer(&mut self, host: &mut dyn Host, timer: TimerKey) -> bool {
        if self.timer != Some(timer) {
            return false;
        }
        self.priming = false;
        self.update(host);
        let seconds = self.common.interval.unwrap_or(f64::MAX / 4.0).max(0.001);
        self.timer = Some(host.timer(Duration::from_secs_f64(seconds.min(1e9))));
        true
    }
}

/// `/proc/meminfo` read into kiB by name.
#[must_use]
pub fn parse_meminfo(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let value = rest.split_whitespace().next()?.parse().ok()?;
            Some((name.to_owned(), value))
        })
        .collect()
}

/// kiB in one `unit`, as waybar's table has them.
fn divisor(unit: &str) -> Option<f32> {
    Some(match unit {
        "B" => 1.0 / 1024.0,
        "kB" => 1000.0 / 1024.0,
        "kiB" => 1.0,
        "MB" => 1_000_000.0 / 1024.0,
        "MiB" => 1024.0,
        "GB" => 1_000_000_000.0 / 1024.0,
        "GiB" => 1024.0 * 1024.0,
        "TB" => 1e12 / 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    })
}

/// The `memory` module.
#[derive(Debug)]
pub struct Memory {
    common: Common,
    unit: String,
    timer: Option<TimerKey>,
    view: ModuleView,
}

impl Memory {
    /// Make it.
    #[must_use]
    pub fn new(name: &str, config: &Value) -> Self {
        let common = Common::new(name, config, "{}%", 30);
        let unit = config
            .get("unit")
            .as_str()
            .filter(|unit| divisor(unit).is_some())
            .unwrap_or("GiB")
            .to_owned();
        let view = common.view("memory", Shape::Label);
        Self {
            common,
            unit,
            timer: None,
            view,
        }
    }

    fn update(&mut self, host: &mut dyn Host) {
        let text = host.read("/proc/meminfo").unwrap_or_default();
        let info = parse_meminfo(&text);
        let get = |name: &str| info.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
        let total = get("MemTotal").unwrap_or(0);
        let swap_total = get("SwapTotal").unwrap_or(0);
        let swap_free = get("SwapFree").unwrap_or(0);
        let free = match get("MemAvailable") {
            Some(available) => available,
            None => (get("MemFree").unwrap_or(0)
                + get("Buffers").unwrap_or(0)
                + get("Cached").unwrap_or(0)
                + get("SReclaimable").unwrap_or(0))
            .saturating_sub(get("Shmem").unwrap_or(0)),
        };
        if total == 0 {
            self.view.visible = false;
            return;
        }
        let used = total.saturating_sub(free);
        let percentage = 100 * used / total;
        let swap_percentage = (100 * swap_total.saturating_sub(swap_free))
            .checked_div(swap_total)
            .unwrap_or(0);
        let by = divisor(&self.unit).unwrap_or(1024.0 * 1024.0);
        #[expect(clippy::cast_precision_loss, reason = "kiB as waybar's float")]
        let hundredths = |kib: u64| 0.01f32 * ((kib as f32 / by) * 100.0).round();
        let state_value = u8::try_from(percentage.min(255)).unwrap_or(255);
        let (state, states) = self.common.state(state_value, false);
        let mut label_format = self.common.format.clone();
        if !state.is_empty()
            && let Some(state_format) = self.common.config.get(&format!("format-{state}")).as_str()
        {
            label_format = state_format.to_owned();
        }
        if label_format.is_empty() {
            self.view.visible = false;
            return;
        }
        self.view.visible = true;
        let percentage = u32::try_from(percentage).unwrap_or(0);
        let args = Args::new()
            .positional(percentage)
            .named(
                "icon",
                self.common
                    .icon(u16::try_from(percentage).unwrap_or(0), &[&state], 0),
            )
            .named("total", Arg::F32(hundredths(total)))
            .named("swapTotal", Arg::F32(hundredths(swap_total)))
            .named("percentage", percentage)
            .named("swapState", if swap_total == 0 { "Off" } else { "On" })
            .named(
                "swapPercentage",
                u32::try_from(swap_percentage).unwrap_or(0),
            )
            .named("used", Arg::F32(hundredths(used)))
            .named(
                "swapUsed",
                Arg::F32(hundredths(swap_total.saturating_sub(swap_free))),
            )
            .named("avail", Arg::F32(hundredths(free)))
            .named("swapAvail", Arg::F32(hundredths(swap_free)));
        let tooltip = format!("{:.1}{} used", hundredths(used), self.unit);
        set_label(
            &mut self.view,
            &self.common,
            host,
            &label_format,
            &tooltip,
            &state,
            &args,
        );
        self.view.classes.retain(|class| !states.contains(class));
        if !state.is_empty() {
            self.view.classes.push(state);
        }
    }
}

impl Module for Memory {
    fn start(&mut self, host: &mut dyn Host) {
        self.update(host);
        let seconds = self.common.interval.unwrap_or(1e9).max(0.001);
        self.timer = Some(host.timer(Duration::from_secs_f64(seconds.min(1e9))));
    }

    fn view(&self) -> &ModuleView {
        &self.view
    }

    fn common(&mut self) -> &mut Common {
        &mut self.common
    }

    fn timer(&mut self, host: &mut dyn Host, timer: TimerKey) -> bool {
        if self.timer != Some(timer) {
            return false;
        }
        self.start(host);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::Module;
    use super::super::fake::Fake;
    use super::{Cpu, Memory, frequencies, load, parse_stat, usage};
    use crate::json::parse;

    #[test]
    fn stat_lines_become_idle_and_total() {
        let stat = "cpu  10 0 10 70 10 0 0 0 0 0\ncpu0 5 0 5 35 5 0 0 0 0 0\ncpu2 5 0 5 35 5 0 0 0 0 0\nintr 1\n";
        let parsed = parse_stat(stat, Some("0-3\n"));
        assert_eq!(parsed, vec![(80, 100), (40, 50), (0, 0), (40, 50), (0, 0)]);
    }

    #[test]
    fn usage_is_the_share_not_idle() {
        let (usages, tooltip) = usage(&[(80, 100), (40, 50)], &[(100, 200), (50, 100)]);
        assert_eq!(usages, vec![80, 80]);
        assert_eq!(tooltip, "Total: 80%\nCore0: 80%");
    }

    #[test]
    fn load_rounds_up_and_says_when_there_is_none() {
        assert_eq!(
            load(Some("0.521 0.2 0.1 1/100 42")),
            ([0.53, 0.2, 0.1], true)
        );
        assert_eq!(load(None), ([0.0; 3], false));
        assert_eq!(
            frequencies(Some("cpu MHz\t\t: 2400.000\ncpu MHz : 3600.5\n")),
            (3.6, 2.4, 3.0)
        );
    }

    #[test]
    fn the_users_cpu_module_on_ferrix() {
        let mut host = Fake::default();
        let _ = host
            .files
            .insert("/proc/stat".into(), "cpu  10 0 10 70 10 0 0 0 0 0\n".into());
        let config = parse(r#"{"format": "cpu {usage}%", "interval": 5, "tooltip-format": "<span line_height='2.0'>  load {load}  </span>"}"#)
            .unwrap_or(crate::json::Value::Null);
        let mut cpu = Cpu::new("cpu", &config);
        cpu.start(&mut host);
        let _ = host.files.insert(
            "/proc/stat".into(),
            "cpu  20 0 20 140 20 0 0 0 0 0\n".into(),
        );
        let (&timer, _) = host
            .timers
            .iter()
            .next()
            .unwrap_or((&0, &std::time::Duration::ZERO));
        assert!(cpu.timer(&mut host, timer));
        // 1 - 80/100 in single precision is 0.19999999, and waybar
        // truncates: 19, not 20.
        assert_eq!(cpu.view().markup, "cpu 19%");
        assert_eq!(
            cpu.view().tooltip.as_deref(),
            Some("<span line_height='2.0'>  load 0  </span>")
        );
        assert!(host.diag.has("no /proc/loadavg"));
        assert_eq!(cpu.view().classes, vec!["module"]);
        assert!(host.timers.values().any(|d| d.as_secs() == 5));
    }

    #[test]
    fn the_users_memory_module() {
        let mut host = Fake::default();
        let _ = host.files.insert(
            "/proc/meminfo".into(),
            "MemTotal:        8388608 kB\nMemFree:  1000 kB\nMemAvailable:    6291456 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n".into(),
        );
        let config = parse(r#"{"format": "ram {percentage}%", "interval": 10, "tooltip-format": "  {used:0.1f}G / {total:0.1f}G  "}"#)
            .unwrap_or(crate::json::Value::Null);
        let mut memory = Memory::new("memory", &config);
        memory.start(&mut host);
        assert_eq!(memory.view().markup, "ram 25%");
        assert_eq!(memory.view().tooltip.as_deref(), Some("  2.0G / 8.0G  "));
        let config = parse(r#"{}"#).unwrap_or(crate::json::Value::Null);
        let mut memory = Memory::new("memory", &config);
        memory.start(&mut host);
        assert_eq!(memory.view().markup, "25%", "{{}} is the percentage");
        assert_eq!(memory.view().tooltip.as_deref(), Some("2.0GiB used"));
    }
}
