//! # Randori's jj engine
//!
//! The repository side of [Randori](https://github.com/jeonsu0609/Randori), taken
//! from [GG](https://github.com/gulbanana/gg): a worker thread that owns a jj-lib
//! workspace and answers queries and mutations, the message types it speaks, jj
//! config loading and the git askpass relay. The UI lives in `crates/randori`.

pub mod askpass;
pub mod config;
pub mod messages;
pub mod worker;

pub use config::read_config;
