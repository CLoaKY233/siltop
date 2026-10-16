#![warn(unreachable_pub)]
// Off macOS every collector is a stub: their helpers and fields are kept for
// symmetry with the real implementations but intentionally never used.
#![cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        unused_imports,
        clippy::unused_self,
        clippy::unnecessary_wraps
    )
)]

mod battery;
mod cadence;
mod collector;
mod cpu;
mod disk;
mod doctor;
mod dvfs;
mod fan;
mod gpu;
mod hid;
#[cfg(target_os = "macos")]
mod iokit;
mod ioreport;
mod memory;
mod needs;
mod net;
mod pacer;
mod power;
mod process;
mod sampler;
mod sensors;
mod smc;
mod smc_keys;
mod soc;
#[cfg(target_os = "macos")]
mod sys;
mod thermal;
mod topology;
mod zones;

pub use cadence::{LOW_POWER_TICK, on_battery, tick_interval};
pub use doctor::{Diagnostics, ProcReadability, SmcInventory, SmcKey, diagnostics};
pub use needs::Needs;
pub use process::{
    ActionError, ActionResult, KillTarget, Signal, can_signal, inspect, is_critical, live_target,
    nice, send_signal, send_signal_all, set_background, set_nice, tree_targets,
};
pub use sampler::Sampler;
