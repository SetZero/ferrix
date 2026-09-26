//! `workspace = <workspace>, <rule>[, <rule>…]`: what one workspace is
//! unlike the others.
//!
//! Hyprland calls the keyword `workspace` and the thing it makes a
//! *workspace rule* (`CConfigManager::handleWorkspaceRules`). The first
//! field names the workspace -- a number, `name:something`, or `special:…`
//! -- and every field after it is a `key:value`:
//!
//! ```text
//! workspace = 1, monitor:DP-1, default:true
//! workspace = name:code, gapsout:0, bordersize:0, layout:master
//! workspace = 5, persistent:true, on-created-empty:foot
//! ```
//!
//! The values are read where they change something the compositor can do:
//! the gaps and the border are the layout's, `monitor` decides which screen
//! the workspace lives on, `persistent` makes it exist with nothing on it,
//! `default` makes it the one a fresh monitor shows, `layout` and
//! `layoutopt` override `general:layout` for this workspace alone, and
//! `on-created-empty` is a command run the first time it is opened with
//! nothing in it.
//!
//! `border`, `shadow`, `rounding` and `decorate` are read as the negations
//! Hyprland stores them as (`border:false` is `no_border`), because that is
//! what `hyprctl workspacerules` prints.

use crate::value::{Gaps, parse_gaps, parse_int};

/// Which workspace a rule is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Which {
    /// `workspace = 3, …`: by number.
    Id(i64),
    /// `workspace = name:code, …`, and any bare word that is not a number,
    /// which Hyprland also takes as a name.
    Name(String),
    /// `workspace = special:magic, …`.
    Special(String),
}

impl Which {
    /// Read the first field of a `workspace =` line.
    fn parse(text: &str) -> Self {
        let text = text.trim();
        if let Some(name) = text.strip_prefix("name:") {
            return Self::Name(name.trim().to_owned());
        }
        if let Some(name) = text.strip_prefix("special:") {
            return Self::Special(name.trim().to_owned());
        }
        if text == "special" {
            return Self::Special("special".to_owned());
        }
        match text.parse() {
            Ok(id) => Self::Id(id),
            Err(_) => Self::Name(text.to_owned()),
        }
    }

    /// How `hyprctl workspacerules` prints it, which is the string the
    /// line was written with.
    #[must_use]
    pub fn as_written(&self) -> String {
        match self {
            Self::Id(id) => id.to_string(),
            Self::Name(name) => format!("name:{name}"),
            Self::Special(name) => format!("special:{name}"),
        }
    }
}

/// One `workspace =` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceRule {
    /// The workspace it is about, as written.
    pub workspace: String,
    /// The same, read.
    pub which: Which,
    /// `gapsin:`: this workspace's `general:gaps_in`.
    pub gaps_in: Option<Gaps>,
    /// `gapsout:`: its `general:gaps_out`.
    pub gaps_out: Option<Gaps>,
    /// `bordersize:`: its `general:border_size`.
    pub border_size: Option<i64>,
    /// `border:false`: its windows get no border at all. Unset where the
    /// line did not say, which `hyprctl workspacerules` prints as
    /// `<unset>` and is not the same as `border:true`.
    pub no_border: Option<bool>,
    /// `shadow:false`.
    pub no_shadow: Option<bool>,
    /// `rounding:false`.
    pub no_rounding: Option<bool>,
    /// `decorate:`: whether its windows are decorated at all.
    pub decorate: Option<bool>,
    /// `monitor:`: which screen it lives on, by name or description.
    pub monitor: Option<String>,
    /// `default:true`: a fresh monitor shows this one.
    pub is_default: Option<bool>,
    /// `persistent:true`: it exists even with nothing on it.
    pub is_persistent: Option<bool>,
    /// `defaultName:`: what it is called when it is made.
    pub default_name: Option<String>,
    /// `on-created-empty:`: a command run the first time it is opened with
    /// nothing on it.
    pub on_created_empty: Option<String>,
    /// `layout:`: `general:layout` for this workspace alone.
    pub layout: Option<String>,
    /// `layoutopt:<name>:<value>`: one of the layout's own options, in the
    /// order they were written.
    pub layout_options: Vec<(String, String)>,
    /// `animation:`: which animation style it uses, recorded as written.
    pub animation: Option<String>,
}

impl Default for Which {
    fn default() -> Self {
        Self::Id(0)
    }
}

impl WorkspaceRule {
    /// Read one `workspace =` line's value.
    ///
    /// # Errors
    ///
    /// Hyprland's own wording for a field it cannot read. A `key:value`
    /// whose key is not one of Hyprland's is *skipped* rather than refused,
    /// which is what `assignRule` does with it: its chain of `find`s falls
    /// through and returns nothing.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (first, rest) = match value.split_once(',') {
            Some((first, rest)) => (first.trim(), rest),
            None => (value.trim(), ""),
        };
        if first.is_empty() {
            return Err(format!("workspace: `{value}` names no workspace"));
        }
        let mut rule = Self {
            workspace: first.to_owned(),
            which: Which::parse(first),
            ..Self::default()
        };
        for field in rest.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            rule.field(field)?;
        }
        Ok(rule)
    }

    /// One `key:value` field.
    fn field(&mut self, field: &str) -> Result<(), String> {
        let Some((key, value)) = field.split_once(':') else {
            // Hyprland's chain of `find`s matches nothing here and the
            // rule is left as it was.
            return Ok(());
        };
        let value = value.trim();
        // Hyprland stores three of these as their negations, because what
        // it turns off is what it has a flag for.
        let off = |value: &str| -> Result<bool, String> { Ok(parse_int(value)? == 0) };
        match key.trim() {
            "gapsin" => self.gaps_in = Some(parse_gaps(value)?),
            "gapsout" => self.gaps_out = Some(parse_gaps(value)?),
            "bordersize" => self.border_size = Some(parse_int(value)?),
            "border" => self.no_border = Some(off(value)?),
            "shadow" => self.no_shadow = Some(off(value)?),
            "rounding" => self.no_rounding = Some(off(value)?),
            "decorate" => self.decorate = Some(parse_int(value)? != 0),
            "monitor" => self.monitor = Some(value.to_owned()),
            "default" => self.is_default = Some(parse_int(value)? != 0),
            "persistent" => self.is_persistent = Some(parse_int(value)? != 0),
            "defaultName" => self.default_name = Some(value.to_owned()),
            "on-created-empty" => self.on_created_empty = Some(value.to_owned()),
            "layout" => self.layout = Some(value.to_owned()),
            "animation" => self.animation = Some(value.to_owned()),
            "layoutopt" => {
                let (name, setting) = value
                    .split_once(':')
                    .ok_or_else(|| format!("Invalid workspace rule found: {field}"))?;
                self.layout_options
                    .push((name.trim().to_owned(), setting.trim().to_owned()));
            }
            // Not one of Hyprland's keys, and Hyprland skips it.
            _ => {}
        }
        Ok(())
    }

    /// Whether this rule is about the workspace named `name` with id `id`.
    ///
    /// A rule written with a number matches by number and one written with
    /// a name matches by name, which is how a person can write
    /// `workspace = 3` and `workspace = name:code` in one file and have
    /// them mean different workspaces.
    #[must_use]
    pub fn matches(&self, id: i64, name: &str) -> bool {
        match &self.which {
            Which::Id(wanted) => *wanted == id,
            Which::Name(wanted) | Which::Special(wanted) => wanted == name,
        }
    }
}

/// What every rule that matched one workspace comes to.
///
/// Later lines win, as they do everywhere else in the configuration.
#[must_use]
pub fn rules_for(rules: &[WorkspaceRule], id: i64, name: &str) -> WorkspaceRule {
    let mut out = WorkspaceRule::default();
    for rule in rules.iter().filter(|rule| rule.matches(id, name)) {
        out.workspace.clone_from(&rule.workspace);
        out.which = rule.which.clone();
        out.gaps_in = rule.gaps_in.or(out.gaps_in);
        out.gaps_out = rule.gaps_out.or(out.gaps_out);
        out.border_size = rule.border_size.or(out.border_size);
        out.no_border = rule.no_border.or(out.no_border);
        out.no_shadow = rule.no_shadow.or(out.no_shadow);
        out.no_rounding = rule.no_rounding.or(out.no_rounding);
        out.decorate = rule.decorate.or(out.decorate);
        out.monitor = rule.monitor.clone().or(out.monitor);
        out.is_default = rule.is_default.or(out.is_default);
        out.is_persistent = rule.is_persistent.or(out.is_persistent);
        out.default_name = rule.default_name.clone().or(out.default_name);
        out.on_created_empty = rule.on_created_empty.clone().or(out.on_created_empty);
        out.layout = rule.layout.clone().or(out.layout);
        out.layout_options
            .extend(rule.layout_options.iter().cloned());
        out.animation = rule.animation.clone().or(out.animation);
    }
    out
}
