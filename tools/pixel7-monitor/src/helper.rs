//! The launcher's helper, `bootloaders/pixel7/launcher/helper.py`, which
//! boots the phone into Ferrix with `fastboot boot`: asked over its HTTP port,
//! and started by this app when it is not running.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Where the helper listens.
const ADDRESS: &str = "127.0.0.1:47707";

/// One request to the helper, and the JSON it answered, or why not.
fn request(method: &str, path: &str) -> Result<serde_json::Value, String> {
    let address: SocketAddr = ADDRESS.parse().map_err(|error| format!("{error}"))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(300))
        .map_err(|_| "the helper is not running".to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {ADDRESS}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).map_err(|error| error.to_string())?;
    let (head, body) = reply.split_once("\r\n\r\n").ok_or("the helper's answer had no body")?;
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status.starts_with('2') {
        Ok(value)
    } else {
        Err(value.get("error").and_then(|e| e.as_str()).unwrap_or(status).to_string())
    }
}

/// What the helper says it is doing, or `None` when it is not running.
pub fn status() -> Option<serde_json::Value> {
    request("GET", "/status").ok()
}

/// Ask the helper to boot the phone into Ferrix.
pub fn boot() -> Result<serde_json::Value, String> {
    request("POST", "/boot")
}

/// The helper's script, in the checkout this app was built from.
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bootloaders/pixel7/launcher/helper.py")
}

/// Start the helper as this app's child, booting `image` or the newest run's.
pub fn start(image: Option<String>) -> Result<Child, String> {
    let mut command = Command::new("python3");
    command.arg(script());
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start the helper: {error}"))
}
