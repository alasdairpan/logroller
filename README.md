# LogRoller 🪵

[![Crates.io](https://img.shields.io/crates/v/logroller)](https://crates.io/crates/logroller)
[![Docs.rs](https://docs.rs/logroller/badge.svg)](https://docs.rs/logroller)
[![License](https://img.shields.io/crates/l/logroller)](https://img.shields.io/crates/l/logroller)
[![CI Status](https://github.com/trayvonpan/logroller/actions/workflows/ci.yml/badge.svg)](https://github.com/trayvonpan/logroller/actions/workflows/ci.yml)

LogRoller is a Rust library for efficient log writing and file rotation. It provides a simple API for writing logs to files and automatically rotating them based on size or time. It works seamlessly with the `tracing-appender` crate for use with the `tracing` framework.

## Table of Contents

- [Features](#features-)
- [Installation](#installation-)
- [Usage](#usage-)
  - [Basic Logger](#1-use-logroller-as-a-simple-logger)
  - [With Tracing Framework](#2-use-logroller-with-tracing-and-tracing-appender)
- [Configuration Options](#configuration-options-)
- [Advanced Examples](#advanced-examples-)
- [Performance Considerations](#performance-considerations-)
- [Troubleshooting](#troubleshooting-)
- [Contributing](#contributing-)

## Features ✨

- **Log Writing**: Write logs to files with ease using standard Write trait implementation
- **File Rotation**:
  - Size-based rotation (bytes, KB, MB, GB)
  - Time-based rotation (minutely, hourly, daily)
- **Configurable**:
  - Customizable rotation strategies
  - Adjustable file retention policies
  - Configurable file permissions
- **Time Zones**:
  - Support for UTC, local, or custom time zones
  - Consistent log timing across different deployment environments
- **Thread-safe**: Designed for safe concurrent usage with proper locking mechanisms
- **Tracing Support**: Seamless integration with [tracing](https://github.com/tokio-rs/tracing) - use LogRoller directly as an appender with [tracing-appender](https://crates.io/crates/tracing-appender)
- **Compression**: Automatic Gzip compression for rotated log files

## Installation 📦

Add `logroller` to your `Cargo.toml`:

```toml
# For basic logging or with tracing framework
[dependencies]
logroller = "0.1"
```

## Usage 🚀

### 1. Use `logroller` as a simple logger

Create a new `LogRoller` instance and write logs to it:

```rust
use logroller::{LogRollerBuilder, Rotation, RotationSize};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut logger = LogRollerBuilder::new("./logs", "logger.log")
        .rotation(Rotation::SizeBased(RotationSize::MB(100)))  // Rotate at 100MB
        .max_keep_files(3)  // Keep only last 3 files
        .build()?;

    writeln!(logger, "This is an info message")?;
    writeln!(logger, "This is a warning message")?;
    writeln!(logger, "This is an error message")?;

    Ok(())
}
```

### 2. Use `logroller` with `tracing` and `tracing-appender`

Integrate with the tracing framework for structured logging:

```rust
use logroller::{Compression, LogRollerBuilder, Rotation, RotationAge, TimeZone};
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let appender = LogRollerBuilder::new("./logs", "tracing.log")
        .rotation(Rotation::AgeBased(RotationAge::Daily))  // Rotate daily
        .max_keep_files(7)  // Keep a week's worth of logs
        .time_zone(TimeZone::Local)  // Use local timezone
        .compression(Compression::Gzip)  // Compress old logs
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

## Configuration Options 🔧

### Rotation Strategies

```rust
// Size-based rotation
Rotation::SizeBased(RotationSize::Bytes(1_000_000))  // 1MB in bytes
Rotation::SizeBased(RotationSize::KB(1_000))        // 1MB in KB
Rotation::SizeBased(RotationSize::MB(1))            // 1MB
Rotation::SizeBased(RotationSize::GB(1))            // 1GB

// Time-based rotation
Rotation::AgeBased(RotationAge::Minutely)  // Rotate every minute
Rotation::AgeBased(RotationAge::Hourly)    // Rotate every hour
Rotation::AgeBased(RotationAge::Daily)     // Rotate at midnight
```

### Time Zones

```rust
use chrono::FixedOffset;

// UTC time zone
.time_zone(TimeZone::UTC)

// Local system time zone
.time_zone(TimeZone::Local)

// Custom time zone (e.g., UTC+8)
.time_zone(TimeZone::Fix(FixedOffset::east_opt(8 * 3600).unwrap()))
```

### File Management

```rust
// Limit number of kept files
.max_keep_files(7)  // Keep only last 7 files

// Add custom suffix to log files
.suffix("error".to_string())  // Results in: app.log.2025-04-01.error

// Set file permissions (Unix systems only)
.file_mode(0o644)  // rw-r--r-- permissions
```

## Advanced Examples 🔍

### High-frequency Logging with Compression

```rust
use logroller::{Compression, LogRollerBuilder, Rotation, RotationAge, TimeZone};

let logger = LogRollerBuilder::new("./logs", "high_freq.log")
    .rotation(Rotation::AgeBased(RotationAge::Hourly))
    .max_keep_files(24)  // Keep one day of hourly logs
    .compression(Compression::Gzip)
    .time_zone(TimeZone::UTC)  // Use UTC for consistent timing
    .build()?;
```

### Size-based Rotation for Large Files

```rust
let logger = LogRollerBuilder::new("./logs", "large_app.log")
    .rotation(Rotation::SizeBased(RotationSize::GB(1)))
    .max_keep_files(5)  // Keep only last 5 files
    .compression(Compression::Gzip)
    .build()?;
```

### Graceful Shutdown

```rust
let mut logger = LogRollerBuilder::new("./logs", "graceful.log")
    .rotation(Rotation::AgeBased(RotationAge::Daily))
    .graceful_shutdown(true)  // Enable graceful shutdown
    .build()?;
```

- LogRoller provides graceful shutdown functionality to ensure proper cleanup of background operations: (default is `false`)
- When `graceful_shutdown` is `true`, `flush()` waits for background compression threads to complete
- When `graceful_shutdown` is `false` (default), `flush()` returns immediately without waiting
- Setting to `false` prevents potential hangs during shutdown but may cause compression corruption


## Performance Considerations 🚀

- Use `tracing-appender::non_blocking` for non-blocking log writes
- Enable compression for rotated files to save disk space
- Consider time-based rotation for predictable file sizes
- Use appropriate rotation sizes to balance between file handling overhead and disk usage
- Set reasonable `max_keep_files` to prevent excessive disk usage

## Troubleshooting 🔧

Common issues and solutions:

1. **File Permissions**:

   - Ensure the process has write permissions to the log directory
   - Use `.file_mode()` to set appropriate permissions on Unix systems

2. **Disk Space**:

   - Monitor disk usage with appropriate `max_keep_files` settings
   - Enable compression for rotated files
   - Use size-based rotation to prevent individual files from growing too large

3. **Time Zone Issues**:
   - Use `TimeZone::UTC` for consistent timing across different environments
   - Be aware of daylight saving time changes when using local time

## Contributing 🤝

Contributions are welcome! Here's how you can help:

1. Fork the repository
2. Create a new branch for your feature
3. Add tests for new functionality
4. Submit a pull request

Please ensure your code:

- Follows the existing code style
- Includes appropriate documentation
- Contains tests for new functionality
- Updates the README.md if needed

## License 📄

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
