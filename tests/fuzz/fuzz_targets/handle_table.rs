//! Fuzz a process's handle table against a model of what it should hold.
//!
//! From stage 9 every native system call starts by looking a handle up, and
//! the handle is whatever number the calling program put in a register. The
//! table's arithmetic — slot from the high bits, generation from the low —
//! runs on all of it, in ring 0, with `overflow-checks` on.
//!
//! # The properties
//!
//! Not crashing is the floor. What the kernel's security rests on is:
//!
//! 1. **A closed handle never resolves again**, whatever was opened since. The
//!    model remembers every value ever closed and the table must refuse every
//!    one of them, after every operation.
//! 2. **The table holds exactly what the model holds** — the same handles with
//!    the same rights naming the same objects — so a refused batch operation
//!    has changed nothing, and a successful one changed exactly what it said.
//! 3. **Rights never grow.** A duplicate or a replacement carries what was
//!    asked for, and only if that was a subset of what was held.
//! 4. **A new handle is non-zero and has never been issued before.**

#![no_main]

use std::collections::{BTreeMap, BTreeSet};

use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::{Requested, Rights};
use ferrix_objects::table::{HandleTable, TableError};
use libfuzzer_sys::fuzz_target;

/// The most operations one input may run, so a long input stays fast.
const MAX_OPS: usize = 512;

/// Reads the input a byte at a time, and runs out as zeros.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> u8 {
        match self.0.split_first() {
            Some((&b, rest)) => {
                self.0 = rest;
                b
            }
            None => 0,
        }
    }

    fn done(&self) -> bool {
        self.0.is_empty()
    }

    fn rights(&mut self) -> Rights {
        Rights(u32::from(self.byte()) & Rights::ALL.0)
    }

    fn requested(&mut self) -> Requested {
        let b = self.byte();
        if b & 0x80 != 0 {
            Requested::Same
        } else {
            Requested::Exactly(Rights(u32::from(b) & Rights::ALL.0))
        }
    }
}

/// What the table should hold.
#[derive(Default)]
struct Model {
    /// Open handles: object and rights.
    live: BTreeMap<Handle, (u64, Rights)>,
    /// Every value ever closed.
    dead: BTreeSet<Handle>,
    /// Every value ever issued, live or dead, for picking from.
    issued: Vec<Handle>,
    /// The next object number.
    next_object: u64,
}

impl Model {
    /// A handle to operate on: usually one that was issued, sometimes an
    /// arbitrary value.
    fn pick(&self, input: &mut Input<'_>) -> Handle {
        let b = input.byte();
        if b >= 0xF0 || self.issued.is_empty() {
            Handle(u32::from_le_bytes([
                input.byte(),
                input.byte(),
                input.byte(),
                b,
            ]))
        } else {
            self.issued[usize::from(b) % self.issued.len()]
        }
    }

    /// Record a handle the table just issued.
    fn opened(&mut self, handle: Handle, object: u64, rights: Rights) {
        assert!(handle.is_valid(), "the table issued zero");
        assert!(
            !self.dead.contains(&handle),
            "{handle:?} was reissued after close"
        );
        assert!(
            !self.live.contains_key(&handle),
            "{handle:?} issued while open"
        );
        let _ = self.live.insert(handle, (object, rights));
        self.issued.push(handle);
    }

    /// Record a handle the table just closed.
    fn closed(&mut self, handle: Handle) -> (u64, Rights) {
        let entry = self
            .live
            .remove(&handle)
            .expect("closed a handle the model lacks");
        let _ = self.dead.insert(handle);
        entry
    }

    /// The result the table should give for looking `handle` up.
    fn expect(&self, handle: Handle) -> Result<(u64, Rights), TableError> {
        self.live.get(&handle).copied().ok_or(TableError::BadHandle)
    }
}

fn check(table: &HandleTable<u64>, model: &Model) {
    let listed: Vec<(Handle, Rights)> = table.handles().collect();
    let expected: Vec<(Handle, Rights)> = model.live.iter().map(|(&h, &(_, r))| (h, r)).collect();
    let mut listed_sorted = listed.clone();
    listed_sorted.sort_unstable_by_key(|(h, _)| *h);
    assert_eq!(listed_sorted, expected, "the table and the model disagree");
    assert_eq!(table.len(), model.live.len(), "len");
    for (&handle, &(object, rights)) in &model.live {
        assert_eq!(table.get(handle), Ok((&object, rights)), "{handle:?}");
    }
    for &handle in &model.dead {
        assert_eq!(
            table.get(handle),
            Err(TableError::BadHandle),
            "stale {handle:?} resolves"
        );
    }
}

fn step(table: &mut HandleTable<u64>, model: &mut Model, input: &mut Input<'_>) {
    match input.byte() % 7 {
        0 => {
            let rights = input.rights();
            let object = model.next_object;
            model.next_object += 1;
            match table.insert(object, rights) {
                Ok(handle) => model.opened(handle, object, rights),
                Err(back) => {
                    assert_eq!(back, object, "a refused insert must give the object back");
                    assert_eq!(table.room(), 0, "refused with room");
                }
            }
        }
        1 => {
            let handle = model.pick(input);
            let expected = model.expect(handle);
            let got = table.remove(handle);
            assert_eq!(got, expected, "remove {handle:?}");
            if got.is_ok() {
                let _ = model.closed(handle);
            }
        }
        2 => {
            let handle = model.pick(input);
            let requested = input.requested();
            match table.duplicate(handle, requested) {
                Ok(copy) => {
                    let (object, held) = model.expect(handle).expect("duplicated a bad handle");
                    assert!(
                        held.contains(Rights::DUPLICATE),
                        "duplicated without the right"
                    );
                    let rights = requested.resolve(held).expect("a duplicate gained rights");
                    assert_eq!(table.get(copy), Ok((&object, rights)), "the copy");
                    model.opened(copy, object, rights);
                }
                Err(TableError::BadHandle) => {
                    assert!(model.expect(handle).is_err(), "refused a good handle")
                }
                Err(_) => {}
            }
        }
        3 => {
            let handle = model.pick(input);
            let requested = input.requested();
            match table.replace(handle, requested) {
                Ok(new) => {
                    let (object, held) = model.closed(handle);
                    let rights = requested
                        .resolve(held)
                        .expect("a replacement gained rights");
                    model.opened(new, object, rights);
                }
                Err(TableError::BadHandle) => {
                    assert!(model.expect(handle).is_err(), "refused a good handle")
                }
                Err(_) => {}
            }
        }
        4 => {
            let count = usize::from(input.byte() % 6);
            let handles: Vec<Handle> = (0..count).map(|_| model.pick(input)).collect();
            let needed = input.rights();
            if let Ok(taken) = table.take_many(&handles, needed) {
                assert_eq!(taken.len(), handles.len(), "took a different number");
                for (&handle, got) in handles.iter().zip(taken) {
                    let expected = model.closed(handle);
                    assert_eq!(got, expected, "took the wrong object for {handle:?}");
                    assert!(expected.1.contains(needed), "took without the right");
                }
            }
        }
        5 => {
            let count = usize::from(input.byte() % 6);
            let objects: Vec<(u64, Rights)> = (0..count)
                .map(|_| {
                    let object = model.next_object;
                    model.next_object += 1;
                    (object, input.rights())
                })
                .collect();
            match table.insert_many(objects.clone()) {
                Ok(handles) => {
                    for (handle, (object, rights)) in handles.into_iter().zip(objects) {
                        model.opened(handle, object, rights);
                    }
                }
                Err(back) => assert_eq!(back, objects, "a refused batch must come back whole"),
            }
        }
        _ => {
            let mut objects = table.clear();
            objects.sort_unstable();
            let mut expected: Vec<u64> = model.live.values().map(|&(o, _)| o).collect();
            expected.sort_unstable();
            assert_eq!(objects, expected, "clear gave back the wrong objects");
            let open: Vec<Handle> = model.live.keys().copied().collect();
            for handle in open {
                let _ = model.closed(handle);
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input(data);
    let limit = usize::from(input.byte() % 65);
    let mut table = HandleTable::new(limit);
    let mut model = Model::default();
    for _ in 0..MAX_OPS {
        if input.done() {
            break;
        }
        step(&mut table, &mut model, &mut input);
        check(&table, &model);
    }
});
