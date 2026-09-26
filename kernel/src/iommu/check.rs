//! The unit gate's waits, in the shapes a unit that answers at once -- every
//! unit QEMU presents -- never puts them in: a look that outlasts its
//! deadline, one that takes long enough for a waiter to start giving up its
//! processor, and an operation that stays inside a gate past the next one's
//! patience.

use super::gate::{self, Gate};

/// Run them.
///
/// # Errors
///
/// The first property that did not hold, as a sentence.
pub(crate) fn run() -> Result<(), &'static str> {
    // A deadline already past: one last look, and its answer.
    if gate::poll(|| false, 0) {
        return Err("a unit wait past its deadline said the unit had answered");
    }

    // A unit that answers on the hundredth look, to a waiter that may block:
    // it gives up its processor between looks and still sees the answer.
    let mut looks = 0_u32;
    let far = crate::timer::now_nanos().saturating_add(5_000_000_000);
    if !gate::poll(
        || {
            looks += 1;
            looks > 100
        },
        far,
    ) {
        return Err("a unit wait that yielded between looks missed the unit's answer");
    }

    // An operation inside the gate for longer than the next one's patience:
    // the next is refused rather than left waiting for good.
    let unit = Gate::new();
    let inside = unit
        .enter()
        .map_err(|_| "an empty gate could not be entered")?;
    // Through the same wait `enter` makes, with a patience of 10 ms, not
    // the unit's second.
    if unit.enter_within(10_000_000).is_ok() {
        return Err("a gate was entered twice at once");
    }
    drop(inside);
    if unit.enter().is_err() {
        return Err("a gate left by its holder could not be entered");
    }
    // As a translated domain's failure report prints it.
    if !alloc::format!("{unit:?}").starts_with("Gate { held: false") {
        return Err("a gate does not print whether it is held");
    }
    Ok(())
}
