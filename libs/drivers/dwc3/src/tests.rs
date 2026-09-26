//! Tests against a model of the controller, its memory seen through a
//! cache, and a Linux host on the cable (`model`), driven by `rig`:
//! bring-up, enumeration and control transfers (`enumeration`), the serial
//! port's data (`data`), what goes wrong (`faults`), and the event
//! encoding on its own (`events`).

mod data;
mod enumeration;
mod events;
mod faults;
mod model;
mod rig;
