//! `clock`: the time (`modules/clock.cpp`), as far as a C locale and one
//! time zone go.
//!
//! `format` (default `{:%H:%M}`) is libfmt's chrono formatting of the local
//! time: `{:…}` takes `strftime`'s conversions. The clock is updated on the
//! `interval` (60 s by default), aligned to the interval as waybar aligns
//! it, so a minute clock ticks on the minute. The zone is the host's local
//! offset.
//!
//! Not carried out, and said once when the config asks for them:
//! `timezone`/`timezones` (other zones than the local one), `locale`,
//! and the `{calendar}` grid (`calendar` options), which is empty here.

use std::time::Duration;

use super::{Common, Host, Module, TimerKey};
use crate::fmt::{Arg, Args, format};
use crate::json::Value;
use crate::view::{ModuleView, Shape};

/// The `clock` module.
#[derive(Debug)]
pub struct Clock {
    common: Common,
    timer: Option<TimerKey>,
    view: ModuleView,
}

impl Clock {
    /// Make it, saying what of its config is not carried out.
    pub fn new(name: &str, config: &Value, host: &mut dyn Host) -> Self {
        let common = Common::new(name, config, "{:%H:%M}", 60);
        for key in ["timezone", "timezones", "locale", "calendar"] {
            if config.has(key) {
                host.diag()
                    .warn(format!("clock: \"{key}\" is not carried out"));
            }
        }
        let view = common.view("clock", Shape::Label);
        Self {
            common,
            timer: None,
            view,
        }
    }

    fn update(&mut self, host: &mut dyn Host) {
        let (epoch, offset) = host.wall();
        let zone = if offset == 0 {
            "UTC".to_owned()
        } else {
            let sign = if offset < 0 { '-' } else { '+' };
            format!(
                "{sign}{:02}{:02}",
                offset.abs() / 3600,
                offset.abs() / 60 % 60
            )
        };
        let time = Arg::Time(epoch + offset, zone);
        let args = Args::new()
            .positional(time)
            .named("calendar", "")
            .named("tz_list", "");
        match format(&self.common.format, &args) {
            Ok(text) => self.view.markup = text,
            Err(error) => host.diag().error(format!("clock: {error}")),
        }
        if self.common.tooltip {
            let tooltip = self.common.tooltip_format("{:%Y %B}", "");
            self.view.tooltip = format(&tooltip, &args).ok();
        }
    }
}

impl Module for Clock {
    fn start(&mut self, host: &mut dyn Host) {
        self.update(host);
        let interval = self.common.interval.unwrap_or(1e9).clamp(1.0, 1e9);
        let (epoch, _) = host.wall();
        // Wake at the next multiple of the interval, as waybar's sleep_until.
        #[expect(clippy::cast_precision_loss, reason = "seconds since the epoch")]
        let now = epoch as f64;
        let next = ((now / interval).floor() + 1.0) * interval;
        self.timer = Some(host.timer(Duration::from_secs_f64((next - now).max(0.001))));
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
    use super::Clock;
    use crate::json::parse;

    #[test]
    fn the_default_clock_ticks_on_the_minute() {
        let mut host = Fake::default();
        host.wall = (1_790_442_245, 0);
        let config = parse("{}").unwrap_or(crate::json::Value::Null);
        let mut clock = Clock::new("clock", &config, &mut host);
        clock.start(&mut host);
        assert_eq!(clock.view().markup, "17:04");
        assert_eq!(clock.view().tooltip.as_deref(), Some("2026 September"));
        assert!(host.timers.values().any(|d| d.as_secs() == 55));
    }
}
