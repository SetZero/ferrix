//! The log control protocol: what a ring-3 driver and the kernel's log core
//! say to each other so that the driver can carry the kernel log off the
//! machine.
//!
//! The one reader it was made for is the Pixel 7's USB serial port
//! (`docs/PIXEL7-USB-HANDOVER.md`, phase 4): its driver, `usbdev`, streams the
//! boot's lines and `ferrix-statd`'s output to the host live. A driver holding
//! a device whose binding may read the log asks for a control channel with
//! `log_control_create`, then asks for bytes with READ and is answered with
//! DATA, one at a time, from the oldest byte the kernel still keeps: a driver
//! that starts late still gets the boot, as far back as the log goes.
//!
//! [`message`] is the bytes: three messages, little-endian, decoded strictly,
//! in `libs/proto/inputctl`'s shape. [`session`] is the kernel's half of the
//! conversation: one READ outstanding at a time, answered once there is at
//! least a byte, and what was lost since the last answer carried to the next.
//!
//! Nothing here sends, waits, reads the log or allocates; the glue does.

#![no_std]
#![forbid(unsafe_code)]

pub mod message;
pub mod session;

#[cfg(test)]
mod tests;
