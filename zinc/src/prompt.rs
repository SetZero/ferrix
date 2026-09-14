//! Prompt expansion: the `%` escapes of zsh's `prompt.c` a plain prompt
//! uses. Colours and grouping escapes produce nothing yet.

use crate::shell::Shell;

fn hostname() -> Vec<u8> {
    std::fs::read("/etc/hostname")
        .or_else(|_| std::fs::read("/proc/sys/kernel/hostname"))
        .map(|h| {
            h.into_iter()
                .take_while(|&c| c != b'\n' && c != b'.')
                .collect()
        })
        .unwrap_or_else(|_| b"localhost".to_vec())
}

/// Expand `%` escapes in `ps`.
pub(crate) fn expand(sh: &Shell, ps: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    let pwd = sh.get(b"PWD").map(|v| v.joined()).unwrap_or_default();
    let home = sh.get(b"HOME").map(|v| v.joined()).unwrap_or_default();
    while let Some(&c) = ps.get(i) {
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        while ps.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let Some(&e) = ps.get(i) else { break };
        i += 1;
        match e {
            b'%' => out.push(b'%'),
            b'#' => {
                // SAFETY: geteuid has no preconditions.
                out.push(if unsafe { libc::geteuid() } == 0 {
                    b'#'
                } else {
                    b'%'
                });
            }
            b'm' | b'M' => out.extend(hostname()),
            b'n' => out.extend(
                sh.get(b"USER")
                    .map(|v| v.joined())
                    .unwrap_or_else(|| b"root".to_vec()),
            ),
            b'?' => out.extend(sh.status.to_string().into_bytes()),
            b'd' | b'/' => out.extend_from_slice(&pwd),
            b'~' => {
                if !home.is_empty() && home != b"/" && pwd.starts_with(&home) {
                    out.push(b'~');
                    out.extend_from_slice(pwd.get(home.len()..).unwrap_or(&[]));
                } else {
                    out.extend_from_slice(&pwd);
                }
            }
            b'c' | b'.' | b'C' => {
                out.extend_from_slice(
                    pwd.rsplit(|&c| c == b'/')
                        .find(|p| !p.is_empty())
                        .unwrap_or(b"/"),
                );
            }
            b'F' | b'K' => {
                if ps.get(i) == Some(&b'{') {
                    i = ps
                        .get(i..)
                        .and_then(|r| r.iter().position(|&c| c == b'}'))
                        .map_or(ps.len(), |p| i + p + 1);
                }
            }
            b'{' | b'}' | b'f' | b'k' | b'B' | b'b' | b'U' | b'u' | b'S' | b's' => {}
            other => {
                out.push(b'%');
                out.push(other);
            }
        }
    }
    out
}
