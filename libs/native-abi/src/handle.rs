//! Handles as a program holds them.
//!
//! A handle is a number that means something only in the process holding it:
//! an index into that process's handle table, naming one kernel object and the
//! rights this process holds over it. Two processes holding the same object
//! hold different numbers, and a number copied from one process to another
//! names nothing — which is what makes a handle a capability rather than a
//! global name. The only way to give a handle to another process is to send it
//! through a channel, which removes it from the sender.
//!
//! How the number is built from a table index is the kernel's business, not
//! the ABI's; `libs/objects` does it. The ABI promises exactly two things:
//! the value is 32 bits wide on every architecture, and zero is never a
//! handle.

/// A handle value, as it appears in a register or in a channel message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct Handle(
    /// The raw value. Zero is [`Handle::INVALID`]; every other value is
    /// whatever the kernel handed out.
    pub u32,
);

impl Handle {
    /// No handle at all.
    ///
    /// Zero, so that a zeroed structure or an unset variable names nothing
    /// rather than whichever object happened to be first in the table.
    pub const INVALID: Handle = Handle(0);

    /// Whether this could name something. Not whether it does: only the
    /// table can say that.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }

    /// A handle read from an argument register.
    ///
    /// # Refused, not truncated
    ///
    /// Flag words are truncated to 32 bits on the way in, because Linux does
    /// that and a 64-bit caller's upper half is whatever the compiler left in
    /// the register. A handle is different: truncating
    /// `0x0000_0001_0000_1001` to `0x1001` would turn garbage into a handle
    /// the caller really holds, and the call would act on an object nobody
    /// named. A value that does not fit is [`Handle::INVALID`], which every
    /// call refuses as a bad handle.
    #[must_use]
    pub const fn from_register(value: u64) -> Handle {
        if value > u32::MAX as u64 {
            Handle::INVALID
        } else {
            Handle(value as u32)
        }
    }
}
