//! `hyprctl`: the request shape, the answers, and the event stream.
//!
//! Hyprland is controlled by two Unix sockets in its instance directory.
//! `.socket.sock` takes one request per connection and answers it;
//! `.socket2.sock` sends a line for every state change and takes nothing.
//! `hyprctl` the program is a client of the first and a bar is a client of
//! the second, so the shape of both is what other people's programs are
//! written against, and matching it is what makes those programs work here.
//!
//! This crate is the shape and nothing else: it holds no socket, so what a
//! request means and what an answer says are host-tested, and the binary
//! above it carries the bytes.
//!
//! # The request
//!
//! A request is one line. Hyprland reads a set of leading flags separated by
//! `/`, then the command and its arguments:
//!
//! ```text
//! clients
//! j/clients
//! [[BATCH]]dispatch workspace 2;dispatch killactive
//! dispatch movefocus l
//! keyword general:gaps_in 10
//! ```
//!
//! `j` asks for JSON rather than the readable form, and `r` asks for it
//! without the pretty-printing. `[[BATCH]]` runs several, separated by `;`,
//! and answers them one after the other. Everything else in Hyprland's flag
//! set -- `a` for all windows, `-` for no trailing newline -- is read and
//! recorded here whether or not a command acts on it, because a flag read as
//! part of a command name is a command nobody can call.
//!
//! # What is answered
//!
//! The commands `hyprctl` is most used for and a bar needs: `version`,
//! `monitors`, `workspaces`, `clients`, `activewindow`, `activeworkspace`,
//! `dispatch`, `keyword`, `reload` and `splash`. What is not answered is
//! answered as Hyprland answers an unknown command -- with a line saying so,
//! not by closing the connection -- so a program that asks for something
//! newer keeps working for everything else it asks.

mod json;
mod reply;
mod request;
mod state;

pub use json::Json;
pub use reply::{Reply, Version, answer};
pub use request::{Flags, Format, Request};
pub use state::{Monitor, Snapshot, Window, Workspace};

/// Hyprland's own socket names inside its instance directory.
///
/// A program looks for `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`
/// and then these, so a compositor that puts them anywhere else is one
/// `hyprctl` cannot find.
pub const REQUEST_SOCKET: &str = ".socket.sock";

/// The event socket's name in the same directory.
pub const EVENT_SOCKET: &str = ".socket2.sock";

#[cfg(test)]
mod tests;
