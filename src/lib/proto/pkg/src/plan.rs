//! Installing a set of packages into one root: whether it may be done, and
//! in which order.
//!
//! The image builder asks this before it writes one file, and so will the
//! package manager. A set is refused, whole, when two packages have one
//! name, when a dependency is missing or too old, when two packages own one
//! path, or when the dependencies go round in a circle -- nothing runs at
//! install time, so an order is only a matter of reading, but a circle is a
//! mistake in a manifest, and saying so costs nothing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::manifest::{Error, refuse};
use crate::record::Record;

/// The order to install `records` in, as indices into it: every package
/// after what it depends on, and otherwise in the order given.
///
/// # Errors
///
/// A name given twice, a dependency missing or older than asked for, a path
/// owned by two packages, or a circle of dependencies.
pub fn plan(records: &[Record]) -> Result<Vec<usize>, Error> {
    let index = |name: &str| {
        records
            .iter()
            .position(|record| record.package.name == name)
    };
    for (at, record) in records.iter().enumerate() {
        let name = &record.package.name;
        if index(name) != Some(at) {
            return refuse(format!("`{name}` is in the set twice"));
        }
        for needed in &record.package.depends {
            let Some(found) = index(&needed.name).and_then(|at| records.get(at)) else {
                return refuse(format!(
                    "`{name}` needs `{needed}`, which is not in the set"
                ));
            };
            if !needed.accepts(&found.package.version) {
                return refuse(format!(
                    "`{name}` needs `{needed}`, and the set has {}",
                    found.package.version
                ));
            }
        }
    }
    no_shared_path(records)?;
    order(records, &index)
}

/// No path is owned by two packages.
fn no_shared_path(records: &[Record]) -> Result<(), Error> {
    let mut owners: Vec<(String, &str)> = Vec::new();
    for record in records {
        for path in record.paths() {
            if let Some((_, owner)) = owners.iter().find(|(owned, _)| *owned == path) {
                return refuse(format!(
                    "`{path}` is installed by both `{owner}` and `{}`",
                    record.package.name
                ));
            }
            owners.push((path, &record.package.name));
        }
    }
    Ok(())
}

/// Dependencies first, by a depth-first walk that refuses a circle.
fn order(records: &[Record], index: &impl Fn(&str) -> Option<usize>) -> Result<Vec<usize>, Error> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        New,
        Walking,
        Done,
    }
    let mut marks = alloc::vec![Mark::New; records.len()];
    let mut out = Vec::with_capacity(records.len());
    // An explicit stack of (package, next dependency to look at), so a long
    // chain costs heap rather than call depth.
    for start in 0..records.len() {
        let mut stack = Vec::from([(start, 0_usize)]);
        while let Some(&(at, next)) = stack.last() {
            let (Some(record), Some(mark)) = (records.get(at), marks.get(at).copied()) else {
                return refuse(format!("package {at} is not in the set"));
            };
            if next == 0 {
                match mark {
                    Mark::Done => {
                        let _ = stack.pop();
                        continue;
                    }
                    Mark::Walking => {
                        return refuse(format!(
                            "`{}` depends on itself, through its dependencies",
                            record.package.name
                        ));
                    }
                    Mark::New => set(&mut marks, at, Mark::Walking),
                }
            }
            match record.package.depends.get(next) {
                Some(needed) => {
                    if let Some(top) = stack.last_mut() {
                        top.1 = next + 1;
                    }
                    let dependency = index(&needed.name)
                        .ok_or_else(|| Error(format!("`{}` is not in the set", needed.name)))?;
                    if marks.get(dependency) == Some(&Mark::Walking) {
                        return refuse(format!(
                            "`{}` depends on itself, through its dependencies",
                            needed.name
                        ));
                    }
                    stack.push((dependency, 0));
                }
                None => {
                    set(&mut marks, at, Mark::Done);
                    out.push(at);
                    let _ = stack.pop();
                }
            }
        }
    }
    Ok(out)
}

fn set<T>(marks: &mut [T], at: usize, mark: T) {
    if let Some(slot) = marks.get_mut(at) {
        *slot = mark;
    }
}
