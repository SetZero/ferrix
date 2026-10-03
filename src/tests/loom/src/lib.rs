//! Models of the orderings two of the kernel's lock-free looks rest on,
//! checked by `loom` over every interleaving and every value the memory model
//! lets a load read, up to a preemption bound (`tests/`).
//!
//! Nothing is here but this note: the models are tests, and they restate the
//! kernel's protocols in `loom`'s atomics, because the kernel cannot be built
//! for the host. Each model names the kernel code it stands for, and each has
//! a control, a copy with the one ordering the argument needs taken away,
//! which `loom` must find failing. Run with `cargo xtask loom`.
#![no_std]
