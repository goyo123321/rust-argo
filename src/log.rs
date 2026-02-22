use std::sync::Once;
use tracing_subscriber::{fmt, EnvFilter};

static INIT: Once = Once::new();

pub fn init_log() {
    INIT.call_once(|| {
        // 如果 RUST_LOG 未设置，但 LOG_LEVEL 存在，则映射
        if std::env::var("RUST_LOG").is_err() {
            if let Ok(level) = std::env::var("LOG_LEVEL") {
                let rust_level = match level.to_lowercase().as_str() {
                    "debug" => "debug",
                    "info" => "info",
                    "warn" | "warning" => "warn",
                    "error" => "error",
                    _ => "info",
                };
                std::env::set_var("RUST_LOG", rust_level);
            } else {
                std::env::set_var("RUST_LOG", "info");
            }
        }

        fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_target(false)
            .with_thread_ids(false)
            .with_file(false)
            .with_line_number(false)
            .init();
    });
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)+) => {
        tracing::debug!($($arg)+)
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)+) => {
        tracing::info!($($arg)+)
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)+) => {
        tracing::warn!($($arg)+)
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)+) => {
        tracing::error!($($arg)+)
    };
}