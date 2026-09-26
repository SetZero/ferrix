//! `network`: the interface the default route leaves by, its address and
//! its traffic (`modules/network.cpp`).
//!
//! waybar learns all of it from rtnetlink: the default route's interface
//! (the lowest metric; `family` picks IPv4 by default), the link's carrier,
//! the addresses, and it polls `/proc/net/dev` for bytes. Here the route is
//! `/proc/net/route`'s, the carrier and the IPv4 address and mask are the
//! `ifreq` ioctls (`SIOCGIFFLAGS`'s `IFF_RUNNING`, `SIOCGIFADDR`,
//! `SIOCGIFNETMASK`), which Ferrix answers as Linux does, and the bytes are
//! `/proc/net/dev`'s. What the module shows follows waybar's states:
//! `disconnected` with no interface or no carrier, `linked` with no address,
//! `ethernet` with no wireless ESSID -- which is every interface on Ferrix,
//! which has no nl80211, so `{essid}`, `{signalStrength}` and the other
//! wireless fields are empty or zero -- and `wifi` never.
//!
//! Bandwidths are the bytes since the last update over the seconds since
//! it, as waybar's `pow_format`s.

use std::net::Ipv4Addr;
use std::time::Duration;

use super::{Common, Host, Module, TimerKey};
use crate::fmt::{Arg, Args, Pow, format};
use crate::json::Value;
use crate::view::{ModuleView, Shape};

/// What the ioctls say of an interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    /// `SIOCGIFFLAGS`.
    pub flags: u32,
    /// `SIOCGIFADDR`, if it has one.
    pub address: Option<Ipv4Addr>,
    /// `SIOCGIFNETMASK`.
    pub netmask: Option<Ipv4Addr>,
}

/// `IFF_RUNNING`: the carrier is up.
pub const IFF_RUNNING: u32 = 0x40;

/// A default route in `/proc/net/route`: its interface, gateway and metric.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// The interface.
    pub interface: String,
    /// The gateway.
    pub gateway: Ipv4Addr,
    /// The metric.
    pub metric: u32,
}

/// The default routes of `/proc/net/route`, best (lowest metric) first.
/// The file's addresses are hexadecimal in the machine's byte order, which
/// is little-endian on every Ferrix target.
#[must_use]
pub fn default_routes(text: &str) -> Vec<Route> {
    const RTF_UP: u32 = 0x1;
    let mut routes: Vec<Route> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (interface, destination, gateway, flags, metric, mask) = (
                fields.first()?,
                fields.get(1)?,
                fields.get(2)?,
                fields.get(3)?,
                fields.get(6)?,
                fields.get(7)?,
            );
            let hex = |text: &str| u32::from_str_radix(text, 16).ok();
            let flags = hex(flags)?;
            if hex(destination)? != 0 || hex(mask)? != 0 || flags & RTF_UP == 0 {
                return None;
            }
            Some(Route {
                interface: (*interface).to_owned(),
                gateway: Ipv4Addr::from(hex(gateway)?.swap_bytes()),
                metric: metric.parse().ok()?,
            })
        })
        .collect();
    routes.sort_by_key(|route| route.metric);
    routes
}

/// `readBandwidthUsage`: received and sent bytes of `interface` in
/// `/proc/net/dev`.
#[must_use]
pub fn bytes(text: &str, interface: &str) -> Option<(u64, u64)> {
    let mut found = None;
    for line in text.lines().skip(2) {
        let Some((name, counts)) = line.split_once(':') else {
            continue;
        };
        if name.trim() != interface {
            continue;
        }
        let counts: Vec<u64> = counts
            .split_whitespace()
            .filter_map(|c| c.parse().ok())
            .collect();
        let (received, sent) = (counts.first().copied()?, counts.get(8).copied()?);
        let (r, s) = found.unwrap_or((0, 0));
        found = Some((r + received, s + sent));
    }
    found
}

/// The interface names in `/proc/net/dev`.
#[must_use]
pub fn interfaces(text: &str) -> Vec<String> {
    text.lines()
        .skip(2)
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_owned()))
        .collect()
}

/// `fnmatch` for the `interface` option: `*` and `?`.
#[must_use]
pub fn glob(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let mut dp = vec![vec![false; name.len() + 1]; pattern.len() + 1];
    if let Some(row) = dp.get_mut(0)
        && let Some(cell) = row.get_mut(0)
    {
        *cell = true;
    }
    for i in 1..=pattern.len() {
        let p = pattern.get(i - 1).copied().unwrap_or_default();
        for j in 0..=name.len() {
            let value = match p {
                '*' => {
                    dp.get(i - 1)
                        .and_then(|r| r.get(j))
                        .copied()
                        .unwrap_or(false)
                        || (j > 0
                            && dp
                                .get(i)
                                .and_then(|r| r.get(j - 1))
                                .copied()
                                .unwrap_or(false))
                }
                '?' => {
                    j > 0
                        && dp
                            .get(i - 1)
                            .and_then(|r| r.get(j - 1))
                            .copied()
                            .unwrap_or(false)
                }
                c => {
                    j > 0
                        && name.get(j - 1) == Some(&c)
                        && dp
                            .get(i - 1)
                            .and_then(|r| r.get(j - 1))
                            .copied()
                            .unwrap_or(false)
                }
            };
            if let Some(cell) = dp.get_mut(i).and_then(|r| r.get_mut(j)) {
                *cell = value;
            }
        }
    }
    dp.get(pattern.len())
        .and_then(|r| r.get(name.len()))
        .copied()
        .unwrap_or(false)
}

/// The `network` module.
#[derive(Debug)]
pub struct Network {
    common: Common,
    timer: Option<TimerKey>,
    totals: Option<(u64, u64)>,
    last: f64,
    previous: (u64, u64),
    state: String,
    view: ModuleView,
}

impl Network {
    /// Make it; the bytes so far are read at once, as waybar's constructor
    /// does, so the first update's rate is of the time since.
    pub fn new(name: &str, config: &Value, host: &mut dyn Host) -> Self {
        let common = Common::new(name, config, "{ifname}", 60);
        let view = common.view("network", Shape::Label);
        let mut network = Self {
            common,
            timer: None,
            totals: None,
            last: host.now(),
            previous: (0, 0),
            state: String::new(),
            view,
        };
        let chosen = network.choose(host).map(|route| route.0);
        let dev = host.read("/proc/net/dev").unwrap_or_default();
        network.totals = chosen.and_then(|name| bytes(&dev, &name));
        network
    }

    /// The interface to show and its gateway.
    fn choose(&self, host: &mut dyn Host) -> Option<(String, Option<Ipv4Addr>)> {
        let routes = default_routes(&host.read("/proc/net/route").unwrap_or_default());
        if let Some(wanted) = self.common.config.get("interface").as_str() {
            let dev = host.read("/proc/net/dev").unwrap_or_default();
            let name = interfaces(&dev)
                .into_iter()
                .find(|name| glob(wanted, name))?;
            let gateway = routes
                .iter()
                .find(|r| r.interface == name)
                .map(|r| r.gateway);
            return Some((name, gateway));
        }
        routes
            .first()
            .map(|route| (route.interface.clone(), Some(route.gateway)))
    }

    /// `Network::update`.
    fn update(&mut self, host: &mut dyn Host) {
        let now = host.now();
        let chosen = self.choose(host);
        let interval = self.common.interval.unwrap_or(60.0);
        let mut elapsed = now - self.last;
        let (mut down, mut up) = self.previous;
        if elapsed >= interval * 0.5 {
            if elapsed <= 0.0 {
                elapsed = interval;
            }
            self.last = now;
            let dev = host.read("/proc/net/dev").unwrap_or_default();
            if let Some((name, _)) = &chosen
                && let Some((received, sent)) = bytes(&dev, name)
            {
                let (r0, s0) = self.totals.unwrap_or((received, sent));
                down = received.saturating_sub(r0);
                up = sent.saturating_sub(s0);
                self.totals = Some((received, sent));
                self.previous = (down, up);
            }
        } else {
            elapsed = interval;
        }
        let info = chosen.as_ref().and_then(|(name, _)| host.interface(name));
        let carrier = info.is_some_and(|i| i.flags & IFF_RUNNING != 0);
        let address = info.and_then(|i| i.address);
        let state = if chosen.is_none() || !carrier {
            "disconnected"
        } else if address.is_none() {
            "linked"
        } else {
            "ethernet"
        };
        let (threshold, _) = self.common.state(0, false);
        let config = &self.common.config;
        let pick = |prefix: &str| -> Option<String> {
            if !threshold.is_empty()
                && let Some(f) = config
                    .get(&format!("{prefix}-{state}-{threshold}"))
                    .as_str()
            {
                return Some(f.to_owned());
            }
            config
                .get(&format!("{prefix}-{state}"))
                .as_str()
                .map(str::to_owned)
        };
        let label_format = if self.common.alt {
            self.common.format.clone()
        } else {
            pick("format")
                .or_else(|| config.get("format").as_str().map(str::to_owned))
                .unwrap_or_else(|| "{ifname}".to_owned())
        };
        let tooltip_format = pick("tooltip-format")
            .or_else(|| config.get("tooltip-format").as_str().map(str::to_owned));
        self.view.classes.retain(|class| *class != self.state);
        self.state = state.to_owned();
        self.view.classes.push(state.to_owned());
        let (name, gateway) = chosen.clone().unwrap_or_default();
        let netmask = info.and_then(|i| i.netmask);
        let cidr = netmask.map_or(0, |mask| u32::from(mask).count_ones());
        let per_second = |value: u64| -> i64 {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a byte count as waybar's double"
            )]
            #[expect(clippy::cast_possible_truncation, reason = "a rate in whole units")]
            let rate = (value as f64 / elapsed) as i64;
            rate
        };
        let bits = |value: u64| Arg::Pow(Pow::new(per_second(value.saturating_mul(8)), "b/s"));
        let args = Args::new()
            .named("essid", "")
            .named("bssid", "")
            .named("signaldBm", 0)
            .named("signalStrength", 0)
            .named("signalStrengthApp", "")
            .named("ifname", name.as_str())
            .named(
                "netmask",
                netmask.map(|m| m.to_string()).unwrap_or_default(),
            )
            .named("netmask6", "")
            .named("ipaddr", address.map(|a| a.to_string()).unwrap_or_default())
            .named("gwaddr", gateway.map(|g| g.to_string()).unwrap_or_default())
            .named("cidr", cidr)
            .named("cidr6", 0)
            .named("frequency", "0.0")
            .named("icon", self.common.icon(0, &[state], 0))
            .named("bandwidthDownBits", bits(down))
            .named("bandwidthUpBits", bits(up))
            .named("bandwidthTotalBits", bits(down + up))
            .named(
                "bandwidthDownOctets",
                Arg::Pow(Pow::new(per_second(down), "o/s")),
            )
            .named(
                "bandwidthUpOctets",
                Arg::Pow(Pow::new(per_second(up), "o/s")),
            )
            .named(
                "bandwidthTotalOctets",
                Arg::Pow(Pow::new(per_second(down + up), "o/s")),
            )
            .named(
                "bandwidthDownBytes",
                Arg::Pow(Pow::new(per_second(down), "B/s")),
            )
            .named(
                "bandwidthUpBytes",
                Arg::Pow(Pow::new(per_second(up), "B/s")),
            )
            .named(
                "bandwidthTotalBytes",
                Arg::Pow(Pow::new(per_second(down + up), "B/s")),
            );
        match format(&label_format, &args) {
            Ok(text) => {
                self.view.visible = !text.is_empty();
                self.view.markup = text;
            }
            Err(error) => host.diag().error(format!("{}: {error}", self.common.name)),
        }
        if self.common.tooltip {
            self.view.tooltip = match tooltip_format {
                Some(tooltip) => format(&tooltip, &args).ok(),
                None => Some(self.view.markup.clone()),
            };
        }
        host.diag().once(
            "network-wifi",
            "network: Ferrix has no nl80211, so the wireless fields ({essid}, {signalStrength}, …) are empty and the state is never wifi".to_owned(),
        );
    }
}

impl Module for Network {
    fn start(&mut self, host: &mut dyn Host) {
        self.update(host);
        self.arm(host);
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

impl Network {
    fn arm(&mut self, host: &mut dyn Host) {
        let seconds = self.common.interval.unwrap_or(1e9).clamp(0.001, 1e9);
        self.timer = Some(host.timer(Duration::from_secs_f64(seconds)));
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::super::Module;
    use super::super::fake::Fake;
    use super::{IFF_RUNNING, Interface, Network, bytes, default_routes, glob};
    use crate::json::parse;

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\neth0\t00000000\t0202000A\t0003\t0\t0\t0\t00000000\t0\t0\t0\neth0\t0002000A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
    const DEV: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0\n  eth0:  125000     100    0    0    0     0          0         0    5000      10    0    0    0     0       0          0\n";

    #[test]
    fn the_default_route_and_its_bytes() {
        let routes = default_routes(ROUTE);
        assert_eq!(routes.len(), 1);
        assert_eq!(
            routes.first().map(|r| r.gateway),
            Some(Ipv4Addr::new(10, 0, 2, 2))
        );
        assert_eq!(bytes(DEV, "eth0"), Some((125_000, 5000)));
        assert!(glob("eth*", "eth0") && glob("e?h0", "eth0") && !glob("wl*", "eth0"));
    }

    #[test]
    fn the_users_network_module_on_ferrix() {
        let mut host = Fake::default();
        let _ = host.interfaces.insert(
            "eth0".into(),
            Interface {
                flags: IFF_RUNNING | 1,
                address: Some(Ipv4Addr::new(10, 0, 2, 15)),
                netmask: Some(Ipv4Addr::new(255, 255, 255, 0)),
            },
        );
        let _ = host.files.insert("/proc/net/route".into(), ROUTE.into());
        let _ = host.files.insert("/proc/net/dev".into(), DEV.into());
        let config = parse(
            r#"{"format-ethernet": "eth {bandwidthDownBits}", "format-disconnected": "offline", "tooltip-format": "  {ifname}  {ipaddr}  ", "interval": 5}"#,
        )
        .unwrap_or(crate::json::Value::Null);
        let mut network = Network::new("network", &config, &mut host);
        host.now = 5.0;
        let dev = DEV.replace("125000", "250000");
        let _ = host.files.insert("/proc/net/dev".into(), dev);
        network.start(&mut host);
        assert_eq!(network.view().markup, "eth 200.0kb/s");
        assert_eq!(
            network.view().tooltip.as_deref(),
            Some("  eth0  10.0.2.15  ")
        );
        assert!(network.view().classes.contains(&"ethernet".to_owned()));
        let _ = host.files.insert("/proc/net/route".into(), String::new());
        host.now = 10.0;
        network.start(&mut host);
        assert_eq!(network.view().markup, "offline");
        assert!(network.view().classes.contains(&"disconnected".to_owned()));
        assert!(!network.view().classes.contains(&"ethernet".to_owned()));
    }
}
