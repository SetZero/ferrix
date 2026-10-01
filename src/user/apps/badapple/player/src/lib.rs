//! Bad Apple!!'s player, the part any host can build and test: what the
//! command line asks for, the clock the picture follows, where the picture
//! goes on a screen, and which frame is due. The program (`main.rs`) puts
//! these to work on a screen and a sound card.

pub mod args;
pub mod clock;
pub mod scale;
pub mod step;

#[cfg(test)]
mod tests;
