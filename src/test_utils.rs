//! Process-global mutex for tests that temporarily set environment variables.
//!
//! `XDG_CONFIG_HOME` is read by `state::config_dir()`, which is called from
//! multiple modules. If two test modules each define their own `Mutex<()>` and
//! each set/restore the env var under their own lock, they only exclude
//! themselves — they do not exclude each other. Anything that touches
//! `XDG_CONFIG_HOME` in a test must hold THIS lock.

use std::sync::Mutex;

pub static XDG_ENV_LOCK: Mutex<()> = Mutex::new(());
