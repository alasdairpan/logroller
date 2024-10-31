# LogRoller 🪵

[![Crates.io](https://img.shields.io/crates/v/logroller)](https://crates.io/crates/logroller)
[![Docs.rs](https://docs.rs/logroller/badge.svg)](https://docs.rs/logroller)
![License](https://img.shields.io/aur/license/logroller)

LogRoller is a Rust library for efficient log writing and file rotation. It provides a simple API for writing logs to files and automatically rotating them based on size or time. And it works seamlessly with the `tracing-appender` crate for use with the `tracing` framework.

## Features ✨

- **Log Writing**: Write logs to files with ease.
- **File Rotation**: Automatically rotate log files based on size or time.
- **Configurable**: Easily customize rotation strategies and settings.
- **Time Zones**: Rotate logs according to the local or UTC time zone.
- **Thread-safe**: Designed for safe concurrent usage.
- **Tracing Support**: Use LogRoller as a file appender for the `tracing` framework.
- **Compression**: Compress rotated log files with Gzip.

## Usage 🚀

Add `logroller` to your `Cargo.toml`:

```toml
[dependencies]
logroller = "0.1"
```

### 1. Use `logroller` as a simple logger

Create a new `LogRoller` instance and write logs to it:

```rust
use logroller::{LogRollerBuilder, Rotation, RotationSize};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut logger = LogRollerBuilder::new("./logs", "logger.log")
        .rotation(Rotation::SizeBased(RotationSize::KB(256)))
        .max_keep_files(3)
        .build()?;

    writeln!(logger, "This is an info message")?;
    writeln!(logger, "This is a warning message")?;
    writeln!(logger, "This is an error message")?;

    Ok(())
}
```

### 2. Use `logroller` with `tracing` and `tracing-appender`

`logroller` can be used as a file appender for the `tracing-appender` crate, by enabling the `tracing` feature:

```toml
[dependencies]
logroller = { version = "0.1", features = ["tracing"] }
```

`tracing-appender` only supports rotating logs according to the UTC time zone. `logroller` can rotate logs according to the local time zone.

```rust
use logroller::{Compression, LogRollerBuilder, Rotation, RotationAge, TimeZone};
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let appender = LogRollerBuilder::new("./logs", "tracing.log")
        .rotation(Rotation::AgeBased(RotationAge::Minutely))
        .max_keep_files(3)
        .time_zone(TimeZone::Local)
        .compression(Compression::Gzip)
        .build()?;
    let (non_blocking, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .finish()
        .try_init()?;

    tracing::info!("This is an info message");
    tracing::warn!("This is a warning message");
    tracing::error!("This is an error message");

    Ok(())
}
```
