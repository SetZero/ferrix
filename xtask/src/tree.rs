//! The tree a gate ran on, named as the first line of its log.
//!
//! A row's evidence is a gate's log, and a log that does not say which commit
//! it came from pins nothing but the word of whoever ran it. So `check`,
//! `test-boot`, `test-shell` and `test-vfs` start by printing `HEAD`, the branch
//! and whether the tree was clean, before any build output.

use std::path::Path;
use std::process::Command;

/// Print the line naming the tree under `root`.
///
/// Never fails a gate: a checkout git cannot read, or a host without git,
/// still gets its line, saying what could not be found out.
pub(crate) fn print_header(root: &Path) {
    let head = git(root, &["rev-parse", "HEAD"]);
    // `symbolic-ref` fails on a detached `HEAD`, which is an answer, not an error.
    let branch = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let changed = git(root, &["status", "--porcelain=v1", "--untracked-files=normal"])
        .map(|status| status.lines().filter(|line| !line.is_empty()).count());
    println!(
        "{}",
        header(head.as_deref(), branch.as_deref(), changed)
    );
}

/// Run git in `root` and answer its trimmed standard output, or nothing when
/// it could not be run or did not succeed.
fn git(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
}

/// The line for a tree at `head` on `branch` with `changed` paths differing
/// from it: each part saying so when it is not known.
fn header(head: Option<&str>, branch: Option<&str>, changed: Option<usize>) -> String {
    let Some(head) = head.filter(|head| !head.is_empty()) else {
        return "xtask: tree unknown (not a git checkout, or git is not on PATH)".to_owned();
    };
    let on = match branch.filter(|branch| !branch.is_empty()) {
        Some(branch) => format!("on {branch}"),
        None => "detached".to_owned(),
    };
    let state = match changed {
        Some(0) => "clean".to_owned(),
        Some(1) => "1 path changed".to_owned(),
        Some(count) => format!("{count} paths changed"),
        None => "cleanliness unknown".to_owned(),
    };
    format!("xtask: tree {head} {on}, {state}")
}

#[cfg(test)]
mod tests {
    use super::header;

    const HEAD: &str = "5ad6d65c8b0e1f2a3b4c5d6e7f8091a2b3c4d5e6";

    #[test]
    fn a_clean_branch_names_head_and_branch() {
        assert_eq!(
            header(Some(HEAD), Some("develop"), Some(0)),
            format!("xtask: tree {HEAD} on develop, clean")
        );
    }

    #[test]
    fn a_detached_head_with_changes_says_both() {
        assert_eq!(
            header(Some(HEAD), None, Some(3)),
            format!("xtask: tree {HEAD} detached, 3 paths changed")
        );
        assert_eq!(
            header(Some(HEAD), Some(""), Some(1)),
            format!("xtask: tree {HEAD} detached, 1 path changed")
        );
    }

    #[test]
    fn what_git_could_not_answer_is_said() {
        assert_eq!(
            header(Some(HEAD), Some("develop"), None),
            format!("xtask: tree {HEAD} on develop, cleanliness unknown")
        );
        assert_eq!(
            header(None, Some("develop"), Some(0)),
            "xtask: tree unknown (not a git checkout, or git is not on PATH)"
        );
    }
}
