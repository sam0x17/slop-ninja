//! Structured logging to stderr (journald under systemd).
//!
//! Events carry fixed names and counts. They never include key material,
//! account addresses, rows, ciphertext, signed bytes, endpoint paths or
//! journal contents. Dependency crates log at `warn` and above only.

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;

use crate::config::{LogConfig, LogFormat, LogLevel};

pub fn init(config: &LogConfig) {
    let level = match config.level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
    };
    let targets = Targets::new()
        .with_default(LevelFilter::WARN.min(level))
        .with_target("slop_ninja_validator", level)
        .with_target("slop_ninja_subnet", level);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false);
    let registry = tracing_subscriber::registry();
    // A second initialization (tests) keeps the first subscriber.
    let _ = match config.format {
        LogFormat::Json => registry
            .with(layer.json().flatten_event(true).with_filter(targets))
            .try_init(),
        LogFormat::Text => registry.with(layer.with_filter(targets)).try_init(),
    };
}
