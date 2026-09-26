//! The host-side probe: what of a real config and stylesheet this waybar
//! does not carry out.
//!
//! It reads the files the way `waybar` itself does ([`config::load`] and
//! [`css::Stylesheet::parse`]), then walks them and says, one line each:
//! every diagnostic waybar would log while loading them, every bar option
//! and module option not carried out ([`options`]), every CSS property that
//! GTK3 accepts and this does not draw, every image kind it does not paint,
//! and every selector part that can never match a node of this bar.

use std::path::Path;

use crate::config::{self, System};
use crate::css::Stylesheet;
use crate::css::property::{Declared, Prop};
use crate::css::selector::{Compound, Pseudo};
use crate::css::value::Image;
use crate::diag::{Diagnostics, Level};
use crate::json::Value;

/// The element names waybar's bar and tooltips are made of.
pub const ELEMENTS: [&str; 6] = ["window", "box", "label", "widget", "tooltip", "image"];

/// Run the probe over `config` and `style` (or the files waybar would
/// find), returning the report's lines.
#[must_use]
pub fn run(config_path: Option<&Path>, style_path: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    let mut diag = Diagnostics::default();
    match config::load(config_path, &System, &mut diag) {
        Ok(loaded) => {
            out.push(format!("config: {}", loaded.path.display()));
            let bars: Vec<&Value> = match &loaded.root {
                Value::Array(items) => items.iter().collect(),
                other => vec![other],
            };
            for (index, bar) in bars.iter().enumerate() {
                report_bar(index, bar, &mut out);
            }
        }
        Err(message) => out.push(format!("config: [error] {message}")),
    }
    for line in diag.drain(Level::Info) {
        out.push(format!("config: {line}"));
    }
    let style = style_path
        .map(Path::to_path_buf)
        .or_else(|| config::find(&["style.css"], &System));
    match style {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => {
                out.push(format!("style: {}", path.display()));
                let sheet = Stylesheet::parse(
                    &text,
                    &path,
                    &|path| std::fs::read_to_string(path).ok(),
                    &mut diag,
                );
                for line in diag.drain(Level::Info) {
                    out.push(format!("style: {line}"));
                }
                report_style(&sheet, &mut out);
            }
            Err(error) => out.push(format!("style: [error] {}: {error}", path.display())),
        },
        None => out.push("style: [error] Missing required resource files".to_owned()),
    }
    out
}

fn report_bar(index: usize, bar: &Value, out: &mut Vec<String>) {
    let prefix = format!("bar {index}");
    if !bar.is_object() {
        out.push(format!("{prefix}: not an object; waybar skips it"));
        return;
    }
    let modules: Vec<String> = ["modules-left", "modules-center", "modules-right"]
        .iter()
        .flat_map(|list| bar.get(list).items().iter().map(Value::as_string))
        .collect();
    out.push(format!("{prefix}: output {}", bar.get("output")));
    out.push(format!("{prefix}: modules {}", modules.join(" ")));
    for name in &modules {
        if !bar.has(name) && !name.starts_with("group/") {
            out.push(format!(
                "{prefix}: module \"{name}\" has no configuration block; it takes its defaults"
            ));
        }
    }
}

fn report_compound(compound: &Compound, place: &str, out: &mut Vec<String>) {
    if let Some(element) = &compound.element
        && !ELEMENTS
            .iter()
            .any(|known| element.eq_ignore_ascii_case(known))
    {
        out.push(format!(
            "style: {place}: element \"{element}\" is no node of this bar; the rule matches nothing"
        ));
    }
    for pseudo in &compound.pseudos {
        match pseudo {
            Pseudo::State(state) if state != "hover" => out.push(format!(
                "style: {place}: :{state} is never set on this bar's nodes"
            )),
            Pseudo::Not(inner) => report_compound(inner, place, out),
            _ => {}
        }
    }
}

fn other_image(image: &Image, at: &str) -> Option<String> {
    match image {
        Image::Other(kind) => Some(format!("style: {at}: image {kind}(): not painted")),
        _ => None,
    }
}

fn report_style(sheet: &Stylesheet, out: &mut Vec<String>) {
    for rule in &sheet.rules {
        let place = rule
            .declarations
            .first()
            .map_or_else(String::new, |d| format!("line {}", d.place.line));
        for selector in &rule.selectors {
            report_compound(&selector.first, &place, out);
            for (_, compound) in &selector.rest {
                report_compound(compound, &place, out);
            }
        }
        for declaration in &rule.declarations {
            let at = format!("line {}", declaration.place.line);
            for declared in &declaration.set {
                match declared {
                    Declared::Value(Prop::Unsupported(name)) => {
                        out.push(format!("style: {at}: property \"{name}\": not carried out"))
                    }
                    Declared::Value(Prop::BackgroundImage(images)) => {
                        out.extend(images.iter().filter_map(|image| other_image(image, &at)));
                    }
                    Declared::Value(Prop::TransitionDuration(list))
                        if list.iter().any(|d| *d > 0.0) =>
                    {
                        out.push(format!(
                            "style: {at}: property \"{}\": partly: a change is drawn at once, not animated",
                            declaration.name
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    out.push(
        "style: GTK's theme is not applied: properties the user's file leaves unset take CSS initial values".to_owned(),
    );
}
