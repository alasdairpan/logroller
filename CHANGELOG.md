# Changelog

All notable changes to LogRoller will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.12] - 2026-07-01

### Added

- strftime template support for custom log filenames in age-based rotation (#19)

### Changed

- Updated MSRV from 1.67 to 1.71

### Fixed

- Generate `Cargo.lock` before security audit in CI

## [0.1.11] - 2026-06-28

### Added

- Background worker to offload post-rotation work for better write-path performance
- Pending sequence support for size-based rotation
- `stat()` filtering: only inspect directory entries matching the log file
  pattern (#16)

### Changed

- Use `&Path` for internal functions; document MSRV as 1.67
- Updated author information and repository links in metadata

### Fixed

- Rotation is now checked *before* writing, so log entries land within correct
  size/time boundaries (#17)
- Removed unnecessary `stat` syscalls and improved file removal safety during
  rotation cleanup

## [0.1.10] - 2025-07-05

### Added

- Optional XZ compression support (`xz` feature flag, powered by `xz2`)
- Graceful shutdown function for safe cleanup on process exit
- Adjustable XZ compression level and parallel processing
- New examples: basic XZ compression, rapid XZ with size-based rotation, and a
  Gzip-vs-XZ comparison

### Changed

- Simplified string formatting in examples; improved documentation clarity throughout

### Fixed

- Typo in rapid compression example doc comment
- Invalid XZ compression level error message and validation logic

## [0.1.9] - 2025-06-19

### Changed

- Removed unsound `make_writer` implementation; improved documentation and
  formatting in `Cargo.toml`, `README.md`, and `lib.rs`

### Fixed

- Log file rollover handling and error management (#9, #10)
- Suppressed clippy warnings for other I/O errors

## [0.1.8] - 2025-04-04

### Added

- Cross-platform CI workflow (build + test on stable and nightly channels) (#8)
- Clippy linting in CI
- CI status badge in README

### Fixed

- Compile error on Windows (#7)

## [0.1.7] - 2025-04-01

### Added

- `file_mode` option to set log file permissions (e.g. `0o644`) (#6)

### Changed

- Enhanced documentation and error-handling guidance

## [0.1.6] - 2025-02-26

### Fixed

- `refresh_writer` parameter handling

## [0.1.5] - 2025-02-25

### Fixed

- Log file deletion logic for age-based rotation

## [0.1.4] - 2025-02-25

### Changed

- Improved log file deletion logic for size-based rotation
- Optimised old-log deletion and log compression path

### Fixed

- Implemented increasing-index log rotation so rotated files follow `.1`, `.2`,
  `…` naming

## [0.1.3] - 2025-02-08

### Added

- Custom suffix support for rotated log files (#2)

### Fixed

- File-pattern regex

## [0.1.2] - 2024-11-29

### Changed

- License changed from GPL-3.0 to MIT
- Time zone type switched to `FixedOffset`; simplified time-retrieval logic (#1)

## [0.1.1] - 2024-11-14

### Added

- `rustfmt.toml` configuration and improved import formatting in examples

### Fixed

- License badge in README now points to crates.io

## [0.1.0] - 2024-10-31

### Added

- Initial public release
- Time-based and age-based log rotation
- Gzip compression support
- `max_keep_files` option with automatic pruning
- `tracing-subscriber` integration via `MakeWriter` trait
- `MakeWriter` implementation for `tracing_subscriber::fmt::writer`
- Example: LogRoller with the `tracing` ecosystem

[Unreleased]: https://github.com/alasdairpan/logroller/compare/0.1.12...HEAD
[0.1.12]: https://github.com/alasdairpan/logroller/compare/0.1.11...0.1.12
[0.1.11]: https://github.com/alasdairpan/logroller/compare/0.1.10...0.1.11
[0.1.10]: https://github.com/alasdairpan/logroller/compare/0.1.9...0.1.10
[0.1.9]: https://github.com/alasdairpan/logroller/compare/0.1.8...0.1.9
[0.1.8]: https://github.com/alasdairpan/logroller/compare/0.1.7...0.1.8
[0.1.7]: https://github.com/alasdairpan/logroller/compare/0.1.6...0.1.7
[0.1.6]: https://github.com/alasdairpan/logroller/compare/0.1.5...0.1.6
[0.1.5]: https://github.com/alasdairpan/logroller/compare/0.1.4...0.1.5
[0.1.4]: https://github.com/alasdairpan/logroller/compare/0.1.3...0.1.4
[0.1.3]: https://github.com/alasdairpan/logroller/compare/0.1.2...0.1.3
[0.1.2]: https://github.com/alasdairpan/logroller/compare/0.1.1...0.1.2
[0.1.1]: https://github.com/alasdairpan/logroller/compare/0.1.0...0.1.1
[0.1.0]: https://github.com/alasdairpan/logroller/releases/tag/0.1.0
