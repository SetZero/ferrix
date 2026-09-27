//! `pulseaudio`: the default sink's volume, mute and description
//! (`modules/pulseaudio.cpp`, `util/audio_backend.cpp`).
//!
//! waybar connects with `pa_context_connect(…, PA_CONTEXT_NOFAIL, …)`: with
//! no server it does not fail but waits for one, and until it has one the
//! module shows its format with waybar's starting values -- volume 0, not
//! muted, no description. The server is found as libpulse finds it:
//! `$PULSE_SERVER`, else `$XDG_RUNTIME_DIR/pulse/native`, else
//! `/run/pulse/native`.
//!
//! With a server, [`super::pulse`] asks it for the default sink and source
//! and listens for their changes; a scroll changes the volume by
//! `scroll-step` up to `max-volume`, unless `on-scroll-up`/`-down` say
//! otherwise, as `Pulseaudio::handleScroll` does.
//!
//! Ferrix's own sound server is not there yet (`docs/AUDIO.md` §5: the
//! PulseAudio-protocol server is U2), so on Ferrix the module shows
//! `vol 0%` exactly as waybar would, says once that no server is there, and
//! looks again every five seconds, as libpulse's `PA_CONTEXT_NOFAIL` does.

use std::time::Duration;

use super::pulse::{Asking, Listening};
use super::{Common, Host, Module, Scroll, TimerKey, WatchKey};
use crate::fmt::{Args, format};
use crate::json::Value;
use crate::view::{ModuleView, Shape};

/// What the server says of the default sink and source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sink {
    /// Volume in percent.
    pub volume: u16,
    /// Muted.
    pub muted: bool,
    /// Its description.
    pub description: String,
    /// Whether it is a Bluetooth device.
    pub bluetooth: bool,
    /// The source's volume.
    pub source_volume: u16,
    /// The source is muted.
    pub source_muted: bool,
    /// The source's description.
    pub source_description: String,
    /// The port's form factor, for `format-icons` (`headphone`, `speaker`…).
    pub port: String,
}

/// The `pulseaudio` module.
#[derive(Debug)]
pub struct Pulseaudio {
    common: Common,
    sink: Sink,
    timer: Option<TimerKey>,
    view: ModuleView,
    server: Option<(Asking, Listening, WatchKey)>,
}

/// Where libpulse looks for the server: `$PULSE_SERVER` (a `unix:` one),
/// `$PULSE_RUNTIME_PATH/pulse/native`, `$XDG_RUNTIME_DIR/pulse/native`,
/// else the system socket.
#[must_use]
pub fn server_path(host: &dyn Host) -> String {
    if let Some(server) = host.var("PULSE_SERVER")
        && let Some(path) = server.strip_prefix("unix:")
    {
        return path.to_owned();
    }
    for variable in ["PULSE_RUNTIME_PATH", "XDG_RUNTIME_DIR"] {
        if let Some(dir) = host.var(variable).filter(|dir| !dir.is_empty()) {
            return format!("{dir}/pulse/native");
        }
    }
    "/run/pulse/native".to_owned()
}

impl Pulseaudio {
    /// Make it.
    pub fn new(name: &str, config: &Value, _host: &mut dyn Host) -> Self {
        let common = Common::new(name, config, "{volume}%", 0);
        let view = common.view("pulseaudio", Shape::Label);
        Self {
            common,
            sink: Sink::default(),
            timer: None,
            view,
            server: None,
        }
    }

    /// Show `sink`, as `Pulseaudio::update` does.
    pub fn show(&mut self, sink: Sink, host: &mut dyn Host) {
        self.sink = sink;
        let config = &self.common.config;
        let mut format_name = "format".to_owned();
        let mut classes: Vec<String> = Vec::new();
        let mut label_format = self.common.format.clone();
        if !self.common.alt {
            if self.sink.bluetooth {
                format_name.push_str("-bluetooth");
                classes.push("bluetooth".to_owned());
            }
            if self.sink.muted {
                if format_name != "format"
                    && !config.get(&format!("{format_name}-muted")).is_string()
                {
                    format_name = "format".to_owned();
                }
                format_name.push_str("-muted");
                classes.push("muted".to_owned());
                classes.push("sink-muted".to_owned());
            }
            let (state, _) = self
                .common
                .state(u8::try_from(self.sink.volume.min(255)).unwrap_or(255), true);
            if !state.is_empty()
                && let Some(f) = config.get(&format!("{format_name}-{state}")).as_str()
            {
                label_format = f.to_owned();
            } else if let Some(f) = config.get(&format_name).as_str() {
                label_format = f.to_owned();
            }
        }
        let mut source_format = "{volume}%".to_owned();
        if self.sink.source_muted {
            classes.push("source-muted".to_owned());
            if let Some(f) = config.get("format-source-muted").as_str() {
                source_format = f.to_owned();
            }
        } else if let Some(f) = config.get("format-source").as_str() {
            source_format = f.to_owned();
        }
        let source = format(
            &source_format,
            &Args::new()
                .named("volume", u32::from(self.sink.source_volume))
                .named("source_desc", self.sink.source_description.as_str()),
        )
        .unwrap_or_default();
        let icon = self.common.icon(self.sink.volume, &[&self.sink.port], 0);
        let args = Args::new()
            .named("desc", self.sink.description.as_str())
            .named("volume", u32::from(self.sink.volume))
            .named("format_source", source.as_str())
            .named("source_volume", u32::from(self.sink.source_volume))
            .named("source_desc", self.sink.source_description.as_str())
            .named("icon", icon.as_str());
        match format(&label_format, &args) {
            Ok(text) => {
                self.view.label_visible = !text.is_empty();
                self.view.markup = text;
            }
            Err(error) => host.diag().error(format!("pulseaudio: {error}")),
        }
        if self.common.tooltip {
            let tooltip = self.common.tooltip_format("", "");
            self.view.tooltip = if tooltip.is_empty() {
                Some(self.sink.description.clone())
            } else {
                format(&tooltip, &args).ok()
            };
        }
        self.view.classes.retain(|c| {
            !matches!(
                c.as_str(),
                "bluetooth" | "muted" | "sink-muted" | "source-muted"
            )
        });
        self.view.classes.extend(classes);
    }

    fn probe(&mut self, host: &mut dyn Host) {
        let path = std::path::PathBuf::from(server_path(host));
        match Asking::open(&path).and_then(|asking| Ok((asking, Listening::open(&path)?))) {
            Ok((mut asking, listening)) => {
                host.diag()
                    .info(format!("pulseaudio: connected to {}", path.display()));
                let watch = host.watch(listening.fd());
                match asking.sink() {
                    Ok(sink) => self.show(sink, host),
                    Err(error) => host.diag().warn(format!("pulseaudio: {error}")),
                }
                self.server = Some((asking, listening, watch));
            }
            Err(error) => {
                host.diag().once(
                    "pulseaudio-server",
                    format!(
                        "pulseaudio: no PulseAudio server at {} ({error}): the module shows waybar's starting values until one appears",
                        path.display()
                    ),
                );
                self.timer = Some(host.timer(Duration::from_secs(5)));
            }
        }
    }

    fn requery(&mut self, host: &mut dyn Host) {
        let answer = self.server.as_mut().map(|(asking, _, _)| asking.sink());
        match answer {
            Some(Ok(sink)) => self.show(sink, host),
            Some(Err(error)) => {
                host.diag()
                    .warn(format!("pulseaudio: the server went away: {error}"));
                self.lost(host);
            }
            None => {}
        }
    }

    fn lost(&mut self, host: &mut dyn Host) {
        if let Some((_, _, watch)) = self.server.take() {
            host.unwatch(watch);
        }
        self.timer = Some(host.timer(Duration::from_secs(5)));
    }
}

impl Module for Pulseaudio {
    fn start(&mut self, host: &mut dyn Host) {
        let sink = self.sink.clone();
        self.show(sink, host);
        self.probe(host);
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
        self.timer = None;
        self.probe(host);
        true
    }

    fn readable(&mut self, host: &mut dyn Host, watch: WatchKey) -> bool {
        let Some((_, listening, mine)) = self.server.as_mut() else {
            return false;
        };
        if *mine != watch {
            return false;
        }
        let (changed, open) = listening.read();
        if !open {
            host.diag()
                .warn("pulseaudio: the server went away".to_owned());
            self.lost(host);
            return true;
        }
        if changed {
            self.requery(host);
        }
        changed
    }

    fn scroll(&mut self, host: &mut dyn Host, scroll: Scroll) -> bool {
        let config = &self.common.config;
        let named = match scroll {
            Scroll::Up => "on-scroll-up",
            Scroll::Down => "on-scroll-down",
            Scroll::Left => "on-scroll-left",
            Scroll::Right => "on-scroll-right",
        };
        if config.get(named).is_string() {
            self.common.run_scroll(host, scroll);
            return false;
        }
        let step = config.get("scroll-step").as_f64().unwrap_or(1.0);
        let max = config.get("max-volume").as_f64().unwrap_or(100.0);
        let Some((asking, _, _)) = self.server.as_mut() else {
            return false;
        };
        let now = f64::from(asking.volume().unwrap_or(0));
        let wanted = match scroll {
            Scroll::Up => (now + step).min(max),
            Scroll::Down => (now - step).max(0.0),
            _ => return false,
        };
        #[expect(clippy::cast_possible_truncation, reason = "a volume in percent")]
        #[expect(clippy::cast_sign_loss, reason = "clamped at zero")]
        let wanted = wanted.round().clamp(0.0, f64::from(u16::MAX)) as u16;
        if let Err(error) = asking.set_volume(wanted) {
            host.diag().warn(format!("pulseaudio: {error}"));
        }
        self.requery(host);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::Module;
    use super::super::fake::Fake;
    use super::{Pulseaudio, Sink};
    use crate::json::parse;

    fn users() -> crate::json::Value {
        parse(r#"{"format": "vol {volume}%", "format-muted": "muted", "format-bluetooth": "bt {volume}%", "scroll-step": 5, "tooltip-format": "  {desc}  "}"#)
            .unwrap_or(crate::json::Value::Null)
    }

    #[test]
    fn with_no_server_it_shows_waybars_starting_values_and_says_so() {
        let mut host = Fake::default();
        let _ = host
            .vars
            .insert("XDG_RUNTIME_DIR".into(), "/run/user/0".into());
        let mut pulse = Pulseaudio::new("pulseaudio", &users(), &mut host);
        pulse.start(&mut host);
        assert_eq!(pulse.view().markup, "vol 0%");
        assert!(
            host.diag
                .has("no PulseAudio server at /run/user/0/pulse/native")
        );
    }

    #[test]
    fn muted_and_bluetooth_pick_their_formats_and_classes() {
        let mut host = Fake::default();
        let mut pulse = Pulseaudio::new("pulseaudio", &users(), &mut host);
        pulse.show(
            Sink {
                volume: 40,
                muted: true,
                description: "Speakers".into(),
                ..Sink::default()
            },
            &mut host,
        );
        assert_eq!(pulse.view().markup, "muted");
        assert!(pulse.view().classes.contains(&"muted".to_owned()));
        assert_eq!(pulse.view().tooltip.as_deref(), Some("  Speakers  "));
        pulse.show(
            Sink {
                volume: 40,
                bluetooth: true,
                ..Sink::default()
            },
            &mut host,
        );
        assert_eq!(pulse.view().markup, "bt 40%");
        assert!(!pulse.view().classes.contains(&"muted".to_owned()));
    }
}
