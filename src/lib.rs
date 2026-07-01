//! # LogRoller
//!
//! LogRoller is a library for rolling over log files based on size or age. It
//! supports various rotation strategies, time zones, and compression methods.
//! **LogRoller integrates seamlessly as an appender for the tracing crate,
//! offering flexible log rotation with time zone support**. Log rotation can be
//! configured for either a fixed time zone or the local system time zone,
//! ensuring logs align consistently with specific regional or user-defined time
//! standards. This feature is particularly useful for applications that require
//! organized logs by time, regardless of server location or daylight saving
//! changes. LogRoller enables precise control over logging schedules, enhancing
//! clarity and organization in log management across various time zones.
//!
//!
//! ## Example
//!
//! ```rust
//! use {
//!    logroller::{Compression, LogRollerBuilder, Rotation, RotationAge, TimeZone},
//!    tracing_subscriber::util::SubscriberInitExt,
//! };
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!    let appender = LogRollerBuilder::new("./logs", "tracing.log")
//!        .rotation(Rotation::AgeBased(RotationAge::Minutely))
//!        .max_keep_files(3)
//!        .time_zone(TimeZone::Local) // Use system local time zone when rotating files
//!        .compression(Compression::Gzip) // Compress rotated files with Gzip
//!        .build()?;
//!    let (non_blocking, _guard) = tracing_appender::non_blocking(appender);
//!    tracing_subscriber::fmt()
//!        .with_writer(non_blocking)
//!        .with_ansi(false)
//!        .with_target(false)
//!        .with_file(true)
//!        .with_line_number(true)
//!        .finish()
//!        .try_init()?;
//!
//!    tracing::info!("This is an info message");
//!    tracing::warn!("This is a warning message");
//!    tracing::error!("This is an error message");
//!
//!    Ok(())
//! }
//! ```
use {
    chrono::{DateTime, FixedOffset, Local, NaiveTime, Timelike, Utc},
    flate2::write::GzEncoder,
    regex::Regex,
    std::{
        fmt::Debug,
        fs::{self, DirEntry, Permissions},
        io::{self, Write as _},
        path::{Path, PathBuf},
        sync::mpsc,
        thread::JoinHandle,
    },
};

#[cfg(feature = "xz")]
use xz2::write::XzEncoder;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// A job sent to the background worker after each rotation.
///
/// Carries the path of the old log file and a clone of the metadata so the
/// worker can compress, shift, and prune independently of the write path.
struct PostRotationJob {
    /// Path of the rotated-out log file that needs processing.
    log_path: PathBuf,
    /// Snapshot of the configuration at rotation time.
    meta: LogRollerMeta,
}

/// Extract a human-readable message from a thread panic payload.
fn join_error_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_string(),
            Err(_) => "unknown panic payload".to_string(),
        },
    }
}

/// Background worker that processes [`PostRotationJob`]s.
///
/// Waits for jobs on the channel receiver. When a job arrives, it calls
/// `process_old_logs`, then drains any accumulated backlog before going back
/// to waiting. Exits after an idle timeout of 5 seconds or when the channel
/// is disconnected.
fn run_rotation_worker(rx: mpsc::Receiver<PostRotationJob>) {
    const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    loop {
        match rx.recv_timeout(IDLE_TIMEOUT) {
            Ok(job) => {
                if let Err(err) = job.meta.process_old_logs(&job.log_path) {
                    eprintln!(
                        "Failed to process old log files for '{}': {}",
                        job.log_path.display(),
                        err
                    );
                }
                // Drain any backlog that piled up while we were working.
                while let Ok(job) = rx.try_recv() {
                    if let Err(err) = job.meta.process_old_logs(&job.log_path) {
                        eprintln!(
                            "Failed to process old log files for '{}': {}",
                            job.log_path.display(),
                            err
                        );
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Defines size thresholds for rotating log files in various units.
///
/// When a log file reaches the specified size, it will be rotated and a new
/// file will be created. This enum provides multiple size units to make
/// configuration more intuitive:
///
/// * `Bytes` - Direct byte count (e.g., 1048576 bytes)
/// * `KB` - Kilobytes (1 KB = 1024 bytes)
/// * `MB` - Megabytes (1 MB = 1024 KB)
/// * `GB` - Gigabytes (1 GB = 1024 MB)
///
/// # Examples
/// ```
/// use logroller::{LogRollerBuilder, Rotation, RotationSize};
///
/// // Rotate when file reaches 100 MB
/// let appender = LogRollerBuilder::new("./logs", "large.log")
///     .rotation(Rotation::SizeBased(RotationSize::MB(100)))
///     .build()
///     .unwrap();
///
/// // Rotate when file reaches 2 GB
/// let appender = LogRollerBuilder::new("./logs", "huge.log")
///     .rotation(Rotation::SizeBased(RotationSize::GB(2)))
///     .build()
///     .unwrap();
/// ```
#[derive(Debug, Clone)]
pub enum RotationSize {
    /// Raw byte count
    Bytes(u64),
    /// Kilobytes (1 KB = 1024 bytes)
    KB(u64),
    /// Megabytes (1 MB = 1024 KB = 1,048,576 bytes)
    MB(u64),
    /// Gigabytes (1 GB = 1024 MB = 1,073,741,824 bytes)
    GB(u64),
}

impl RotationSize {
    /// Get the size of the log file in bytes.
    fn bytes(&self) -> u64 {
        match self {
            RotationSize::Bytes(b) => *b,
            RotationSize::KB(kb) => kb * 1024,
            RotationSize::MB(mb) => mb * 1024 * 1024,
            RotationSize::GB(gb) => gb * 1024 * 1024 * 1024,
        }
    }
}

/// Specifies the compression algorithm to use for rotated log files.
///
/// Currently supports Gzip compression, with planned support for additional
/// compression algorithms in future releases. When a log file is rotated,
/// it will be compressed using the specified algorithm and given an appropriate
/// file extension (e.g., `.gz` for Gzip).
///
/// Future compression options may include:
/// * Bzip2 - Higher compression ratio but slower than Gzip
/// * LZ4 - Fast compression with good compression ratio
/// * Zstd - Modern algorithm balancing speed and compression
/// * Snappy - Very fast compression, developed by Google
#[derive(Debug, Clone)]
pub enum Compression {
    /// Gzip compression, which provides a good balance of compression ratio
    /// and speed. Compressed files will have the `.gz` extension.
    Gzip,
    // Bzip2,
    // LZ4,
    // Zstd,
    /// XZ compression.
    ///
    /// Offers the highest compression ratio but significantly slower processing
    /// speed. Accepts a compression level from `0` to `9`:
    /// - `0`: Minimal compression, fastest speed, smallest memory usage.
    /// - `9`: Maximum compression, slowest speed, highest memory usage.
    ///
    /// Higher compression levels require larger dictionary sizes and more RAM.
    /// Ensure that the compression level is within the valid range to avoid
    /// runtime errors.
    ///
    /// **Note:** Requires the `xz` feature to be enabled.
    #[cfg(feature = "xz")]
    XZ(u32),
    // Snappy,
}

impl Compression {
    /// Get the extension for the compressed log file.
    fn get_extension(&self) -> &'static str {
        match self {
            Compression::Gzip => "gz",
            // Compression::Bzip2 => "bz2",
            // Compression::LZ4 => "lz4",
            // Compression::Zstd => "zst",
            #[cfg(feature = "xz")]
            Compression::XZ(_) => "xz",
            // Compression::Snappy => "snappy",
        }
    }
}

/// Specifies the time zone to use for log file rotation and log file naming.
///
/// This setting affects:
/// - When age-based rotation occurs (based on the selected time zone)
/// - How timestamps are formatted in log file names
/// - Ensuring consistent log timing across different deployment environments
///
/// # Examples
/// ```
/// use logroller::TimeZone;
/// use chrono::FixedOffset;
///
/// // Use UTC time for global deployments
/// let utc = TimeZone::UTC;
///
/// // Use local system time zone (changes with system settings)
/// let local = TimeZone::Local;
///
/// // Use a fixed offset for a specific region (e.g., UTC+8 for China)
/// let china = TimeZone::Fix(FixedOffset::east_opt(8 * 3600).unwrap());
/// ```
#[derive(Debug, Clone)]
pub enum TimeZone {
    /// Use UTC time zone. Best for consistent timing in distributed systems
    /// or when deploying across multiple regions.
    UTC,
    /// Use the system's local time zone. Suitable for single-location
    /// deployments where logs should align with local time.
    Local,
    /// Use a fixed time zone offset. Useful for targeting specific regions or
    /// when you need logs to match a particular time zone regardless of where
    /// the application runs.
    Fix(FixedOffset),
}

/// Specifies how frequently log files should be rotated when using age-based
/// rotation.
///
/// This determines the granularity of log file splitting, affecting:
/// - How often new log files are created
/// - The timestamp format in rotated file names
/// - The chronological organization of log data
#[derive(Debug, Clone)]
pub enum RotationAge {
    /// Create a new log file every minute.
    /// File names will include year, month, day, hour, and minute
    /// (e.g., log.2025-04-01-19-55).
    /// Best for high-volume logging where fine-grained splitting is needed.
    Minutely,
    /// Create a new log file every hour.
    /// File names will include year, month, day, and hour
    /// (e.g., log.2025-04-01-19).
    /// Good for moderate-volume logging with hourly aggregation.
    Hourly,
    /// Create a new log file every day at midnight in the configured time zone.
    /// File names will include year, month, and day
    /// (e.g., log.2025-04-01).
    /// Suitable for most applications with standard daily log rotation.
    Daily,
}

/// Defines the strategy for when log files should be rotated.
///
/// LogRoller supports two main rotation strategies:
/// 1. Size-based: Rotate when a file reaches a certain size
/// 2. Age-based: Rotate at fixed time intervals
///
/// Each strategy has different benefits and use cases:
/// - Size-based is good for controlling disk space usage
/// - Age-based is better for time-based organization and retention
#[derive(Debug, Clone)]
pub enum Rotation {
    /// Rotate log files when they reach a specified size.
    /// The rotated files are numbered sequentially (e.g., log.1, log.2).
    /// This helps prevent individual log files from becoming too large
    /// and ensures consistent file sizes.
    SizeBased(RotationSize),
    /// Rotate log files based on time intervals.
    /// Files are named with timestamps according to the specified interval.
    /// This creates a clear chronological organization of log files
    /// and makes it easy to locate logs from specific time periods.
    AgeBased(RotationAge),
}

/// Metadata for the log roller.
/// This struct is used to configure the log roller.
#[derive(Clone)]
struct LogRollerMeta {
    /// The directory where the log files are stored.
    directory: PathBuf,
    /// The name of the log file.
    filename: PathBuf,
    /// The rotation strategy for the log files, determining when and how files
    /// are rotated. Can be either size-based (rotate when file reaches
    /// certain size) or age-based (rotate at specific time intervals).
    rotation: Rotation,
    /// The time zone used for age-based rotation timing and log file naming.
    /// This is stored as a FixedOffset to ensure consistent timing behavior.
    /// For UTC or local time zones, this is converted from the respective time
    /// zone at initialization.
    time_zone: FixedOffset,
    /// The compression type for the log files.
    compression: Option<Compression>,
    /// The maximum number of log files to keep.
    max_keep_files: Option<u64>,
    /// Optional suffix to append to log file names before any extension.
    /// This is primarily used with age-based rotation to help identify log
    /// files with special characteristics (e.g., ".error" for error logs).
    suffix: Option<String>,
    /// The file permissions to set on newly created log files (Unix-like
    /// systems only). This is specified in octal notation (e.g., 0o644 for
    /// rw-r--r--). On non-Unix systems, this setting is ignored with a
    /// warning message.
    file_mode: Option<u32>,
    /// Waits for both compression and old file cleanup during shutdown.
    graceful_shutdown: bool,
    /// Pre-compiled regex that matches archive file names for the current
    /// rotation strategy. Computed once during build.
    file_pattern: Option<Regex>,
    /// When true, `filename` contains strftime tokens and is used as a format
    /// string for age-based rotation paths. When false, the date pattern is
    /// appended after the filename.
    is_filename_template: bool,
}

/// State for the log roller.
/// This struct is used to keep track of the current state of the log roller.
struct LogRollerState {
    /// Monotonically increasing counter giving each pending size-based
    /// rotation a unique identity in the `{filename}.pending.{N}` namespace.
    /// This lets us rotate while a previous compression is still running
    /// — the pending file name is stable and won't be clobbered by a
    /// subsequent rotation's index-shifting dance.
    next_pending_sequence: u64,
    /// The next time for age-based rotation.
    next_age_based_time: DateTime<FixedOffset>,
    /// The current file path.
    curr_file_path: PathBuf,
    /// The current file size in bytes.
    curr_file_size_bytes: u64,
}

impl LogRollerState {
    /// Get the next pending sequence for size-based rotation.
    /// This function will scan the directory for existing pending log files
    /// with the same name and return the next sequence number based on the
    /// existing files. If no existing pending files are found, the sequence
    /// will be set to 0.
    /// Pending files use the format `{filename}.pending.{N}` and are a
    /// temporary identity for queued post-rotation work. The worker later
    /// publishes them into the public numbered archive namespace.
    /// # Arguments
    /// * `directory` - The directory where the log files are stored.
    /// * `filename` - The name of the log file.
    /// # Returns
    /// The next pending sequence number for the log file.
    fn get_next_pending_sequence(directory: &Path, filename: &Path) -> u64 {
        let mut max_suffix: u64 = 0;

        // This is redundant since std::fs::read_dir already check for directory
        if !directory.is_dir() {
            return max_suffix;
        }
        let prefix = format!("{}.pending.", filename.to_string_lossy());
        if let Ok(files) = std::fs::read_dir(directory) {
            for file in files.flatten() {
                if let Some(name) = file.file_name().to_str() {
                    if let Some(suffix_str) = name.strip_prefix(&prefix) {
                        if let Ok(suffix) = suffix_str.parse::<u64>() {
                            max_suffix = std::cmp::max(max_suffix, suffix);
                        }
                    }
                }
            }
        }
        max_suffix + 1
    }

    /// Get the current size of the log file.
    /// This function will return the size of the log file in bytes.
    /// If the log file does not exist, the size will be set to 0.
    /// # Arguments
    /// * `log_path` - The path to the log file.
    /// # Returns
    /// The size of the log file in bytes.
    fn get_curr_size_based_file_size(log_path: &Path) -> u64 {
        std::fs::metadata(log_path).map_or(0, |m| m.len())
    }
}

/// A log roller that rolls over logs based on size or age.
pub struct LogRoller {
    /// Immutable configuration (cheaply cloned for the worker).
    meta: LogRollerMeta,
    /// Mutable runtime state — current file path, size, next rotation time.
    state: LogRollerState,
    /// The active log file.
    writer: fs::File,
    /// Sender half of the post-rotation channel. Dropping this signals the
    /// worker to drain and exit.
    worker_tx: Option<mpsc::Sender<PostRotationJob>>,
    /// Handle to the post-rotation background thread (spawned lazily).
    worker_handle: Option<JoinHandle<()>>,
}

impl LogRoller {
    /// Decide whether the current log file should be rotated.
    ///
    /// For size-based rotation this compares `current_size + pending_bytes`
    /// against the threshold. For age-based rotation it checks whether the
    /// current time has passed the next scheduled rotation boundary.
    ///
    /// Returns `Some(new_log_path)` if rotation is needed, or `None` otherwise.
    #[inline]
    fn should_rollover(meta: &LogRollerMeta, state: &LogRollerState, pending_bytes: u64) -> Option<PathBuf> {
        match &meta.rotation {
            Rotation::SizeBased(rotation_size) => {
                if state.curr_file_size_bytes.saturating_add(pending_bytes) >= rotation_size.bytes() {
                    return Some(meta.directory.join(PathBuf::from(
                        format!("{}.1", meta.filename.as_path().to_string_lossy(),).to_string(),
                    )));
                }
            }
            Rotation::AgeBased(rotation_age) => {
                let now = meta.now();
                let next_time = state.next_age_based_time;
                if now >= next_time {
                    return Some(meta.get_next_age_based_log_path(rotation_age, &next_time));
                }
            }
        }
        None
    }

    /// Submit a [`PostRotationJob`] to the background worker.
    ///
    /// If no worker is running (first rotation), one is spawned. If the worker
    /// has exited (idle timeout), the old thread is reaped and a new one
    /// started. The job is re-submitted so it is never silently dropped.
    fn post_rotation(&mut self, log_path: PathBuf) {
        let meta = self.meta.clone();

        if let Some(tx) = self.worker_tx.take() {
            match tx.send(PostRotationJob { log_path, meta }) {
                Ok(()) => {
                    self.worker_tx = Some(tx);
                }
                Err(mpsc::SendError(job)) => {
                    // Worker exited — reap it.
                    Self::join_worker(&mut self.worker_handle, "worker restart");
                    // Spawn a fresh worker and retry with the recovered job.
                    let (tx, rx) = mpsc::channel();
                    self.worker_handle = Some(std::thread::spawn(move || run_rotation_worker(rx)));
                    self.worker_tx = Some(tx.clone());
                    if let Err(mpsc::SendError(job)) = tx.send(job) {
                        eprintln!(
                            "Failed to resubmit post-rotation job for '{}' after restarting worker",
                            job.log_path.display()
                        );
                    }
                }
            }
        } else {
            // First rotation — spawn the worker.
            let (tx, rx) = mpsc::channel();
            self.worker_handle = Some(std::thread::spawn(move || run_rotation_worker(rx)));
            self.worker_tx = Some(tx.clone());
            if let Err(mpsc::SendError(job)) = tx.send(PostRotationJob { log_path, meta }) {
                eprintln!(
                    "Failed to submit post-rotation job for '{}' to a newly spawned worker",
                    job.log_path.display()
                );
            }
        }
    }

    /// Join the worker thread, logging any panic with a context label.
    fn join_worker(handle: &mut Option<JoinHandle<()>>, context: &str) {
        if let Some(h) = handle.take() {
            if let Err(err) = h.join() {
                eprintln!(
                    "Rotation worker panicked during {context}: {}",
                    join_error_to_string(err)
                );
            }
        }
    }
}

impl LogRollerMeta {
    /// Get the current time in the specified time zone.
    fn now(&self) -> DateTime<FixedOffset> {
        Utc::now().with_timezone(&self.time_zone)
    }

    /// Replace the time in the datetime with the specified time.
    /// # Arguments
    /// * `base_datetime` - The base datetime.
    /// * `time_to_replaced` - The time to be replaced.
    /// # Returns
    /// The datetime with the time replaced.
    #[allow(deprecated)]
    fn replace_time(&self, base_datetime: DateTime<FixedOffset>, time_to_replaced: NaiveTime) -> DateTime<FixedOffset> {
        DateTime::<FixedOffset>::from_local(
            base_datetime.date_naive().and_time(time_to_replaced),
            *base_datetime.offset(),
        )
    }

    /// Get the next time for the log file rotation.
    /// This function will return the next time for the log file rotation based
    /// on the rotation age.
    /// # Arguments
    /// * `base_datetime` - The base datetime.
    /// * `rotation_age` - The rotation age.
    /// # Returns
    /// The next time for the log file rotation.
    fn next_time(
        &self,
        base_datetime: DateTime<FixedOffset>,
        rotation_age: RotationAge,
    ) -> Result<DateTime<FixedOffset>, LogRollerError> {
        match rotation_age {
            RotationAge::Minutely => {
                let d = base_datetime + chrono::Duration::minutes(1);
                Ok(self.replace_time(
                    d,
                    NaiveTime::from_hms_opt(d.hour(), d.minute(), 0).ok_or(LogRollerError::GetNaiveTimeFailed)?,
                ))
            }
            RotationAge::Hourly => {
                let d = base_datetime + chrono::Duration::hours(1);
                Ok(self.replace_time(
                    d,
                    NaiveTime::from_hms_opt(d.hour(), 0, 0).ok_or(LogRollerError::GetNaiveTimeFailed)?,
                ))
            }
            RotationAge::Daily => {
                let d = base_datetime + chrono::Duration::days(1);
                Ok(self.replace_time(
                    d,
                    NaiveTime::from_hms_opt(0, 0, 0).ok_or(LogRollerError::GetNaiveTimeFailed)?,
                ))
            }
        }
    }

    /// Create a new log file.
    /// This function will create a new log file at the specified path.
    /// If the log file already exists, the function will append to the existing
    /// log file. If the log file does not exist, the function will create a
    /// new log file. If the directory does not exist, the function will
    /// create the directory.
    /// # Arguments
    /// * `log_path` - The path to the log file.
    /// # Returns
    /// The log file.
    fn create_log_file(&self, log_path: &Path) -> Result<fs::File, LogRollerError> {
        let mut open_options = fs::OpenOptions::new();
        open_options.append(true).create(true);

        let mut create_log_file_res = open_options.open(log_path);
        if create_log_file_res.is_err() {
            // Create the directory if it doesn't exist
            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| LogRollerError::CreateDirectoryFailed(parent.to_path_buf(), err.to_string()))?;
                create_log_file_res = open_options.open(log_path);
            }
        }

        let log_file = create_log_file_res
            .map_err(|err| LogRollerError::CreateFileFailed(log_path.to_path_buf(), err.to_string()))?;

        self.set_permissions(log_path)?;

        Ok(log_file)
    }

    /// Entry point for all post-rotation work. Dispatches on rotation strategy:
    /// size-based -> publish into numbered archive namespace; age-based ->
    /// compress the old file and prune excess archives.
    fn process_old_logs(&self, log_path: &Path) -> Result<(), LogRollerError> {
        match &self.rotation {
            Rotation::SizeBased(_) => self.publish_size_based_log(log_path),
            Rotation::AgeBased(_) => {
                self.compress(log_path)?;

                if let Some(max_keep_files) = self.max_keep_files {
                    let all_log_files = Self::list_all_files(
                        &self.directory,
                        self.file_pattern.as_ref().expect("file_pattern not initialized"),
                    )?;

                    // Only prune fully-processed files. If compression is
                    // enabled, uncompressed files are still in the worker
                    // queue (or are the active file) and must not be counted.
                    let processed: Vec<_> = match &self.compression {
                        Some(c) => all_log_files
                            .iter()
                            .filter(|f| {
                                f.file_name()
                                    .to_string_lossy()
                                    .ends_with(&format!(".{}", c.get_extension()))
                            })
                            .collect(),
                        None => all_log_files.iter().collect(),
                    };

                    if processed.len() > max_keep_files as usize {
                        for file in processed.iter().take(processed.len() - max_keep_files as usize) {
                            let path = file.path();
                            if let Err(err) = Self::remove_file_if_exists(&path) {
                                eprintln!("Failed to remove old log file '{}': {}", path.display(), err);
                            }
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// List all log files in the directory.
    /// List all log files in the directory matching `file_pattern`.
    /// Results are sorted alphabetically by file name.
    fn list_all_files(directory: &Path, file_pattern: &Regex) -> Result<Vec<DirEntry>, LogRollerError> {
        let files = match fs::read_dir(directory) {
            Ok(files) => files,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(LogRollerError::InternalError(err.to_string())),
        };

        let mut all_log_files = Vec::new();
        for file in files.flatten() {
            // Check the cheap regex match first — only call metadata()
            // (a filesystem syscall) on entries that match the pattern.
            let file_name = file.file_name();
            let file_name_str = match file_name.to_str() {
                Some(name) => name,
                None => continue,
            };
            if !file_pattern.is_match(file_name_str) {
                continue;
            }
            let metadata = match file.metadata() {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(LogRollerError::FileIOError(err)),
            };
            if !metadata.is_file() {
                continue;
            }
            all_log_files.push(file);
        }

        all_log_files.sort_by_key(|f| f.file_name());
        Ok(all_log_files)
    }

    /// Compress the contents of `log_path` and write them to `compressed_path`.
    /// The original file is left in place; the caller is responsible for
    /// removing it if desired.
    fn compress_to_path(&self, log_path: &Path, compressed_path: &Path) -> Result<(), LogRollerError> {
        let compression = match &self.compression {
            Some(compression) => compression,
            None => return Ok(()),
        };
        let infile = fs::File::open(log_path).map_err(LogRollerError::FileIOError)?;
        let mut reader = io::BufReader::new(infile);
        let outfile = fs::File::create(compressed_path).map_err(LogRollerError::FileIOError)?;
        let writer = outfile;

        match compression {
            Compression::Gzip => {
                let mut encoder = GzEncoder::new(writer, flate2::Compression::default());
                io::copy(&mut reader, &mut encoder)?;
                encoder.finish()?;
            }
            #[cfg(feature = "xz")]
            Compression::XZ(level) => {
                let mut encoder = XzEncoder::new(writer, *level);
                io::copy(&mut reader, &mut encoder)?;
                encoder.finish()?;
            }
        }
        self.set_permissions(compressed_path)?;
        Ok(())
    }

    /// Compress `log_path` in-place: write a compressed sibling, then remove
    /// the original. Used for age-based rotation.
    fn compress(&self, log_path: &Path) -> Result<(), LogRollerError> {
        let compression = match &self.compression {
            Some(c) => c,
            None => return Ok(()),
        };
        let compressed_path = PathBuf::from(format!(
            "{}.{}",
            log_path.to_string_lossy(),
            compression.get_extension()
        ));
        self.compress_to_path(log_path, &compressed_path)?;
        Self::remove_file_if_exists(log_path)?;
        Ok(())
    }

    /// Rename `from` -> `to` if `from` exists. Otherwise do nothing.
    fn rename_if_exists(from: &Path, to: &Path) -> Result<(), LogRollerError> {
        match fs::rename(from, to) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LogRollerError::RenameFileError {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
                error: err.to_string(),
            }),
        }
    }

    /// Remove `path` if it exists. Otherwise do nothing.
    fn remove_file_if_exists(path: &Path) -> Result<(), LogRollerError> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LogRollerError::FileIOError(err)),
        }
    }

    /// Return the numbered archive path (e.g. `app.log.1`).
    fn get_size_based_archive_path(&self, index: usize) -> PathBuf {
        self.directory
            .join(format!("{}.{index}", self.filename.to_string_lossy()))
    }

    /// Return the path for a compressed numbered archive (e.g. `app.log.1.gz`).
    fn get_size_based_compressed_archive_path(&self, index: usize, compression: &Compression) -> PathBuf {
        self.directory.join(format!(
            "{}.{index}.{}",
            self.filename.to_string_lossy(),
            compression.get_extension()
        ))
    }

    /// Temporary path used while compressing a pending file before it is
    /// atomically renamed into the archive namespace.
    fn get_size_based_compression_temp_path(&self, pending_log_path: &Path, compression: &Compression) -> PathBuf {
        PathBuf::from(format!(
            "{}.{}.tmp",
            pending_log_path.to_string_lossy(),
            compression.get_extension()
        ))
    }

    /// Parse the numeric archive index from a size-based file name.
    ///
    /// Handles plain archives (`app.log.1` -> `Some(1)`) and compressed
    /// archives (`app.log.1.gz` -> `Some(1)`). Returns `None` for filenames
    /// with fewer than two dot-separated parts.
    fn parse_size_based_archive_index(file_name: &str) -> Option<usize> {
        let mut parts: Vec<&str> = file_name.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        if parts.last().and_then(|segment| segment.parse::<usize>().ok()).is_none() {
            parts.pop();
        }
        parts.last().and_then(|segment| segment.parse::<usize>().ok())
    }

    /// Find the highest archive index currently on disk for size-based
    /// rotation.
    fn max_size_based_archive_index(&self) -> Result<usize, LogRollerError> {
        let all_log_files = Self::list_all_files(
            &self.directory,
            self.file_pattern.as_ref().expect("file_pattern not initialized"),
        )?;

        let mut max_index = 0;
        for file in all_log_files {
            if let Some(index) = file
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(Self::parse_size_based_archive_index)
            {
                max_index = max_index.max(index);
            }
        }
        Ok(max_index)
    }

    /// Shift all existing numbered archives up by one index so that position
    /// `1` is free for the newly rotated file. Both uncompressed and compressed
    /// archives are shifted together.
    fn shift_size_based_archives(&self) -> Result<(), LogRollerError> {
        let max_index = self.max_size_based_archive_index()?;
        for idx in (1..=max_index).rev() {
            let source_file = self.get_size_based_archive_path(idx);
            let target_file = self.get_size_based_archive_path(idx + 1);
            Self::rename_if_exists(&source_file, &target_file)?;

            if let Some(compression) = &self.compression {
                let compressed_source_file = self.get_size_based_compressed_archive_path(idx, compression);
                let compressed_target_file = self.get_size_based_compressed_archive_path(idx + 1, compression);
                Self::rename_if_exists(&compressed_source_file, &compressed_target_file)?;
            }
        }
        Ok(())
    }

    /// Remove archive files whose index exceeds `max_keep_files`.
    fn prune_size_based_archives(&self) -> Result<(), LogRollerError> {
        let Some(max_keep_files) = self.max_keep_files else {
            return Ok(());
        };

        let all_log_files = Self::list_all_files(
            &self.directory,
            self.file_pattern.as_ref().expect("file_pattern not initialized"),
        )?;

        for file in all_log_files {
            if let Some(index) = file
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(Self::parse_size_based_archive_index)
            {
                if index > max_keep_files as usize {
                    let path = file.path();
                    if let Err(err) = Self::remove_file_if_exists(&path) {
                        eprintln!("Failed to remove old log file '{}': {}", path.display(), err);
                    }
                }
            }
        }
        Ok(())
    }

    /// Process a pending size-based rotation: compress (if configured),
    /// shift existing archives up by one, rename the pending file into
    /// position `1`, and prune anything beyond `max_keep_files`.
    fn publish_size_based_log(&self, pending_log_path: &Path) -> Result<(), LogRollerError> {
        match &self.compression {
            Some(compression) => {
                let compressed_pending_path = self.get_size_based_compression_temp_path(pending_log_path, compression);
                self.compress_to_path(pending_log_path, &compressed_pending_path)?;

                self.shift_size_based_archives()?;

                let target_path = self.get_size_based_compressed_archive_path(1, compression);
                fs::rename(&compressed_pending_path, &target_path).map_err(|err| LogRollerError::RenameFileError {
                    from: compressed_pending_path.clone(),
                    to: target_path.clone(),
                    error: err.to_string(),
                })?;

                // Only delete the pending file after the archive rename
                // has succeeded — keeps the data recoverable if the shift
                // or rename fails.
                Self::remove_file_if_exists(pending_log_path)?;
            }
            None => {
                self.shift_size_based_archives()?;

                let target_path = self.get_size_based_archive_path(1);
                Self::rename_if_exists(pending_log_path, &target_path)?;
            }
        }
        self.prune_size_based_archives()
    }

    /// Set the permissions for a file based on the configured file mode.
    ///
    /// This function sets file permissions based on the `file_mode`
    /// configuration. The function only has an effect when:
    /// 1. A file mode has been configured (via `file_mode` builder option)
    /// 2. Running on a Unix-like operating system
    ///
    /// # Arguments
    /// * `path` - The path to the file whose permissions should be set
    ///
    /// # Returns
    /// * `Ok(())` - If permissions were successfully set or if no permissions
    ///   needed to be set
    /// * `Err(LogRollerError)` - If setting permissions failed
    ///
    /// # Platform-specific behavior
    /// * On Unix systems: Sets the file mode using the octal permissions (e.g.,
    ///   0o644 for rw-r--r--)
    /// * On non-Unix systems: Prints a warning message and does nothing, as
    ///   file permissions are platform-specific and the Unix permission model
    ///   doesn't apply
    ///
    /// # Example
    /// ```ignore
    /// let builder = LogRollerBuilder::new("./logs", "app.log")
    ///     .file_mode(0o644)  // Owner can read/write, others can read
    ///     .build()?;
    /// ```
    fn set_permissions(&self, path: &Path) -> Result<(), LogRollerError> {
        if let Some(mode) = self.file_mode {
            #[cfg(unix)]
            {
                let perms = Permissions::from_mode(mode);
                fs::set_permissions(path, perms).map_err(|err| LogRollerError::SetFilePermissionsError {
                    path: path.to_path_buf(),
                    error: err.to_string(),
                })?
            }
            #[cfg(not(unix))]
            {
                eprintln!("Warning: Setting file permissions is not supported on non-Unix platforms");
            }
        }
        Ok(())
    }

    /// Refresh the writer.
    /// This function will refresh the writer by creating a new log file and
    /// compressing the old log file. The function will also rename the
    /// Swap the writer from `old_log_path` to `rotated_log_path`. Flushes
    /// the old file, creates the new one, and returns the path of the old
    /// file for the post-rotation compression thread.
    ///
    /// For size-based rotation, `rotated_log_path` is the pending path
    /// (`file.pending.{N}`). The caller is responsible for spawning the
    /// compression thread that calls `process_old_logs`, which handles
    /// archive shifting, final rename, and pruning.
    fn refresh_writer(
        &self,
        writer: &mut fs::File,
        old_log_path: PathBuf,
        rotated_log_path: PathBuf,
    ) -> Result<PathBuf, LogRollerError> {
        match &self.rotation {
            Rotation::SizeBased(_) => {
                let curr_log_path = self.directory.join(&self.filename);

                // Move the current log to a stable pending path. The
                // compression thread owns publication into the numbered
                // archive namespace.
                std::fs::rename(&curr_log_path, &rotated_log_path).map_err(|err| LogRollerError::RenameFileError {
                    from: curr_log_path.clone(),
                    to: rotated_log_path.clone(),
                    error: err.to_string(),
                })?;

                let new_log_file = match self.create_log_file(&curr_log_path) {
                    Ok(file) => file,
                    Err(err) => {
                        eprintln!("Failed to create new log file '{}': {}", curr_log_path.display(), err);
                        return Err(err);
                    }
                };

                if let Err(err) = writer.flush() {
                    eprintln!("Failed to flush writer: {err}");
                    return Err(LogRollerError::FileIOError(err));
                }
                *writer = new_log_file;

                Ok(rotated_log_path)
            }
            Rotation::AgeBased(_) => {
                let new_log_file = match self.create_log_file(&rotated_log_path) {
                    Ok(file) => file,
                    Err(err) => {
                        eprintln!(
                            "Failed to create new log file '{}': {}",
                            rotated_log_path.display(),
                            err
                        );
                        return Err(err);
                    }
                };

                if let Err(err) = writer.flush() {
                    eprintln!("Failed to flush writer for '{}': {}", rotated_log_path.display(), err);
                    return Err(LogRollerError::FileIOError(err));
                }
                *writer = new_log_file;

                Ok(old_log_path)
            }
        }
    }
}

impl LogRollerMeta {
    /// Create a new log roller metadata.
    /// # Arguments
    /// * `directory` - The directory where the log files are stored.
    /// * `filename` - The name of the log file.
    /// # Returns
    /// The log roller metadata.
    /// The rotation type is set to age-based with daily rotation by default.
    /// The time zone is set to local by default.
    /// The compression type is set to None by default.
    /// The maximum number of log files to keep is set to None by default.
    fn new<P: AsRef<Path>>(directory: P, filename: P) -> Self {
        LogRollerMeta {
            directory: directory.as_ref().to_path_buf(),
            filename: filename.as_ref().to_path_buf(),
            rotation: Rotation::AgeBased(RotationAge::Daily),
            compression: None,
            max_keep_files: None,
            time_zone: Local::now().offset().to_owned(),
            suffix: None,
            file_mode: None,
            graceful_shutdown: false,
            file_pattern: None,
            is_filename_template: false,
        }
    }

    /// Build the regex pattern used to match archive file names for the
    /// current rotation strategy. Called once during build.
    fn build_file_pattern(
        filename: &str,
        rotation: &Rotation,
        file_suffix: &Option<String>,
        compression: &Option<Compression>,
        is_filename_template: bool,
    ) -> Result<Regex, LogRollerError> {
        let file_suffix = file_suffix.as_ref().map(|s| format!("(.{s})?")).unwrap_or_default();
        let compression_suffix = compression
            .as_ref()
            .map(|c| format!("(.{})?", c.get_extension()))
            .unwrap_or_default();

        match rotation {
            Rotation::SizeBased(_) => {
                let escaped_filename = regex::escape(filename);
                Regex::new(&format!(r"^{escaped_filename}\.\d+{file_suffix}{compression_suffix}$"))
                    .map_err(|err| LogRollerError::InternalError(err.to_string()))
            }
            Rotation::AgeBased(rotation_age) => {
                let pattern = if is_filename_template {
                    Self::template_to_regex(filename)
                } else {
                    let escaped_filename = regex::escape(filename);
                    let date_pattern = match rotation_age {
                        RotationAge::Minutely => r"\d{4}-\d{2}-\d{2}-\d{2}-\d{2}",
                        RotationAge::Hourly => r"\d{4}-\d{2}-\d{2}-\d{2}",
                        RotationAge::Daily => r"\d{4}-\d{2}-\d{2}",
                    };
                    format!("{escaped_filename}\\.{date_pattern}")
                };
                Regex::new(&format!(r"^{pattern}{file_suffix}{compression_suffix}$"))
                    .map_err(|err| LogRollerError::InternalError(err.to_string()))
            }
        }
    }

    /// Returns `true` if `s` contains chrono strftime tokens (e.g. `%Y`, `%m`).
    fn has_strftime_tokens(s: &str) -> bool {
        s.contains('%')
    }

    /// Map a strftime token character to a granularity level.
    ///
    /// 1 = year, 2 = month, 3 = day, 4 = hour, 5 = minute, 6 = second.
    fn token_level(token: char) -> Option<u32> {
        match token {
            'Y' => Some(1),
            'm' => Some(2),
            'd' => Some(3),
            'H' => Some(4),
            'M' => Some(5),
            'S' => Some(6),
            _ => None,
        }
    }

    /// Validate a strftime template filename.
    ///
    /// Checks:
    /// - No path separators (sub-directories are not supported).
    /// - Every `%` is followed by a recognised strftime token.
    /// - The finest token granularity is at least as fine as the rotation age
    ///   (e.g. an hourly rotation needs `%H` or finer to avoid filename
    ///   collisions within the same hour).
    fn validate_template(template: &str, rotation_age: &RotationAge) -> Result<(), LogRollerError> {
        if template.contains('/') || template.contains('\\') {
            return Err(LogRollerError::InvalidFilenameTemplate(
                "filename template must not contain path separators".into(),
            ));
        }

        let mut finest_level: u32 = 0;
        let mut finest_token: Option<char> = None;
        let mut chars = template.chars();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some(tok) => match Self::token_level(tok) {
                        Some(level) => {
                            if level > finest_level {
                                finest_level = level;
                                finest_token = Some(tok);
                            }
                        }
                        None => {
                            return Err(LogRollerError::InvalidFilenameTemplate(format!(
                                "unsupported strftime token: %{tok}"
                            )));
                        }
                    },
                    None => {
                        return Err(LogRollerError::InvalidFilenameTemplate(
                            "stray '%' at end of filename template".into(),
                        ));
                    }
                }
            }
        }

        let required = match rotation_age {
            RotationAge::Daily => 3,
            RotationAge::Hourly => 4,
            RotationAge::Minutely => 5,
        };

        if finest_level == 0 {
            return Err(LogRollerError::InvalidFilenameTemplate(
                "filename contains '%' but no recognised strftime tokens".into(),
            ));
        }
        if finest_level < required {
            let age_name = match rotation_age {
                RotationAge::Daily => "daily",
                RotationAge::Hourly => "hourly",
                RotationAge::Minutely => "minutely",
            };
            let needed_token = match rotation_age {
                RotationAge::Daily => "%d",
                RotationAge::Hourly => "%H",
                RotationAge::Minutely => "%M",
            };
            let found = finest_token.map_or_else(String::new, |t| format!("%{t}"));
            return Err(LogRollerError::InvalidFilenameTemplate(format!(
                "filename template needs {needed_token} (or finer) for {age_name} rotation, \
                 but the finest token found is {found}",
            )));
        }
        Ok(())
    }

    /// Convert a strftime template into a regex pattern.
    ///
    /// Maps strftime tokens to digit patterns (`%Y` -> `\d{4}`,
    /// `%m`/`%d`/`%H`/`%M`/`%S` -> `\d{2}`) and regex-escapes literal
    /// characters. Used to build the archive-matching regex for age-based
    /// rotation with template filenames.
    fn template_to_regex(template: &str) -> String {
        let mut regex = String::new();
        let mut chars = template.chars();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('Y') => regex.push_str(r"\d{4}"),
                    Some('m' | 'd' | 'H' | 'M' | 'S') => regex.push_str(r"\d{2}"),
                    Some(other) => {
                        // Defensive: shouldn't happen after validation.
                        regex.push_str(&regex::escape(&format!("%{other}")));
                    }
                    None => break,
                }
            } else {
                regex.push_str(&regex::escape(&ch.to_string()));
            }
        }
        regex
    }

    /// Get the next log file path based on the rotation age.
    fn get_next_age_based_log_path(&self, rotation_age: &RotationAge, datetime: &DateTime<FixedOffset>) -> PathBuf {
        let mut tf = if self.is_filename_template {
            datetime
                .format(self.filename.as_path().to_string_lossy().as_ref())
                .to_string()
        } else {
            let pattern = match rotation_age {
                RotationAge::Minutely => "%Y-%m-%d-%H-%M",
                RotationAge::Hourly => "%Y-%m-%d-%H",
                RotationAge::Daily => "%Y-%m-%d",
            };
            datetime
                .format(&format!("{}.{pattern}", self.filename.as_path().to_string_lossy()))
                .to_string()
        };
        if let Some(suffix) = &self.suffix {
            tf = format!("{tf}.{suffix}");
        }
        self.directory.join(PathBuf::from(tf))
    }

    /// Return the temporary path for a pending size-based rotation.
    /// Pending files use the format `{filename}.pending.{sequence}` so each
    /// rotation gets a unique, stable identity that won't collide with
    /// subsequent rotations.
    fn get_pending_log_path(&self, sequence: u64) -> PathBuf {
        self.directory
            .join(format!("{}.pending.{sequence}", self.filename.to_string_lossy()))
    }

    /// Get the current log file path.
    fn get_curr_log_path(&self) -> PathBuf {
        match &self.rotation {
            Rotation::SizeBased(_) => self.directory.join(self.filename.as_path()),
            Rotation::AgeBased(rotation_age) => self.get_next_age_based_log_path(rotation_age, &self.now()),
        }
    }
}

/// Errors that can occur when using the log roller.
#[derive(Debug, thiserror::Error)]
pub enum LogRollerError {
    #[error("Failed to create directory '{0}': {1}")]
    CreateDirectoryFailed(PathBuf, String),
    #[error("Failed to create file '{0}': {1}")]
    CreateFileFailed(PathBuf, String),
    #[error("Failed to get native time: Invalid time format")]
    GetNaiveTimeFailed,
    #[error("Invalid rotation type: {0}")]
    InvalidRotationType(String),
    #[error("Failed to get next file path for '{0}'")]
    GetNextFilePathError(PathBuf),
    #[error("Failed to rename file from '{from}' to '{to}': {error}")]
    RenameFileError { from: PathBuf, to: PathBuf, error: String },
    #[error("File IO error: {0}")]
    FileIOError(#[from] std::io::Error),
    #[error("Should not rotate log file '{0}' at this time")]
    ShouldNotRotate(PathBuf),
    #[error("Internal error: {0}")]
    InternalError(String),
    #[error("Invalid filename template: {0}")]
    InvalidFilenameTemplate(String),
    #[error("Failed to set file permissions for '{path}': {error}")]
    SetFilePermissionsError { path: PathBuf, error: String },
    #[cfg(feature = "xz")]
    #[error("Invalid XZ compression level {level}. Must be 0 ≤ n ≤ 9")]
    InvalidXZCompressionLevel { level: u32 },
}

/// Provides a fluent interface for configuring LogRoller instances.
///
/// The builder pattern allows for flexible configuration of log rolling
/// behavior, with sensible defaults that can be overridden as needed.
/// Configuration options include:
///
/// * Time zone - Control when rotations occur
/// * Rotation strategy - Choose between size-based or time-based rotation
/// * Compression - Optionally compress rotated files to save space
/// * File retention - Limit the number of historical log files to keep
/// * File naming - Add custom suffixes to help identify different log types
/// * Permissions - Set specific file permissions (Unix systems only)
///
/// # Default Configuration
///
/// If not explicitly configured, LogRoller uses these defaults:
/// * Daily rotation at midnight
/// * Local system time zone
/// * No compression
/// * Keep all historical files
/// * Standard file permissions
///
/// # Examples
///
/// Basic configuration for daily log rotation:
/// ```rust
/// use logroller::{LogRollerBuilder, Rotation, RotationAge};
///
/// let appender = LogRollerBuilder::new("./logs", "app.log")
///     .rotation(Rotation::AgeBased(RotationAge::Daily))
///     .build()
///     .unwrap();
/// ```
///
/// Advanced configuration with multiple options:
/// ```rust
/// use logroller::{Compression, LogRollerBuilder, Rotation, RotationAge, TimeZone};
///
/// let appender = LogRollerBuilder::new("./logs", "app.log")
///     .rotation(Rotation::AgeBased(RotationAge::Hourly))
///     .time_zone(TimeZone::UTC)  // Use UTC for consistent timing
///     .max_keep_files(24)        // Keep one day's worth of hourly logs
///     .compression(Compression::Gzip)  // Compress old logs
///     // .compression(Compression::XZ(6))  // Compress using XZ. Requires `xz` feature.
///     .suffix("error".to_string())  // Name format: app.log.2025-04-01-19.error
///     .build()
///     .unwrap();
/// ```
///
/// Size-based rotation for large log files:
/// ```rust
/// use logroller::{LogRollerBuilder, Rotation, RotationSize};
///
/// let appender = LogRollerBuilder::new("./logs", "large_app.log")
///     .rotation(Rotation::SizeBased(RotationSize::MB(100)))  // Rotate at 100MB
///     .max_keep_files(5)  // Keep only the 5 most recent files
///     .build()
///     .unwrap();
/// ```
pub struct LogRollerBuilder {
    meta: LogRollerMeta,
}

impl LogRollerBuilder {
    /// Create a new log roller builder.
    /// # Arguments
    /// * `directory` - The directory where the log files are stored.
    /// * `filename` - The name of the log file.
    ///
    /// For age-based rotation, the filename may contain a subset of chrono
    /// [strftime tokens](https://docs.rs/chrono/latest/chrono/format/strftime/index.html)
    /// to control how timestamps appear in rotated file names (only `%Y`, `%m`, `%d`, `%H`,
    /// `%M`, `%S` are supported).
    ///
    /// E.g. `%Y-%m-%d.log` produces `2026-06-29.log`.
    /// When no `%` tokens are present, the date is appended to the filename
    /// (e.g. `app.log` -> `app.log.2026-06-29`).
    pub fn new<P: AsRef<Path>>(directory: P, filename: P) -> Self {
        LogRollerBuilder {
            meta: LogRollerMeta::new(directory, filename),
        }
    }

    /// Set the time zone for the log files.
    pub fn time_zone(self, time_zone: TimeZone) -> Self {
        Self {
            meta: LogRollerMeta {
                time_zone: match time_zone {
                    TimeZone::UTC => Utc::now().fixed_offset().offset().to_owned(),
                    TimeZone::Local => Local::now().offset().to_owned(),
                    TimeZone::Fix(fixed_offset) => fixed_offset,
                },
                ..self.meta
            },
        }
    }

    /// Set the rotation type for the log files.
    pub fn rotation(self, rotation: Rotation) -> Self {
        Self {
            meta: LogRollerMeta { rotation, ..self.meta },
        }
    }

    /// Set the compression type for the log files.
    pub fn compression(self, compression: Compression) -> Self {
        Self {
            meta: LogRollerMeta {
                compression: Some(compression),
                ..self.meta
            },
        }
    }

    /// Set the maximum number of log files to keep.
    pub fn max_keep_files(self, max_keep_files: u64) -> Self {
        Self {
            meta: LogRollerMeta {
                max_keep_files: Some(max_keep_files),
                ..self.meta
            },
        }
    }

    /// Set the suffix for the log file.
    pub fn suffix(self, suffix: String) -> Self {
        Self {
            meta: LogRollerMeta {
                suffix: Some(suffix),
                ..self.meta
            },
        }
    }

    /// Set the file permissions for log files (Unix-like systems only).
    /// This sets the file mode bits in octal notation like when using chmod.
    /// For example, 0o644 for rw-r--r-- permissions.
    pub fn file_mode(self, mode: u32) -> Self {
        Self {
            meta: LogRollerMeta {
                file_mode: Some(mode),
                ..self.meta
            },
        }
    }

    /// Determines whether the application should attempt a graceful shutdown.
    /// When set to `true`, the application will perform cleanup operations and
    /// allow in-progress tasks (like file compression and old file cleanup) to
    /// complete before shutting down. If set to `false`, the application may
    /// terminate immediately without waiting for these ongoing tasks.
    ///
    /// **Compression Corruption Risk**: If `graceful_shutdown` is `false` and
    /// the application exits while a compression thread is still writing a
    /// compressed file, the resulting file may be incomplete or corrupted.
    /// Setting this to `true` ensures that all compression operations
    /// finish, but may cause a slight delay during application shutdown.
    ///
    /// By default, this is set to `false`.
    pub fn graceful_shutdown(self, graceful_shutdown: bool) -> Self {
        Self {
            meta: LogRollerMeta {
                graceful_shutdown,
                ..self.meta
            },
        }
    }

    /// Build the log roller.
    pub fn build(mut self) -> Result<LogRoller, LogRollerError> {
        // Detect and validate strftime templates in the filename.
        // Templates only apply to age-based rotation.
        let filename_str = self.meta.filename.as_path().to_string_lossy();
        let is_template = LogRollerMeta::has_strftime_tokens(filename_str.as_ref());

        if is_template {
            match &self.meta.rotation {
                Rotation::AgeBased(rotation_age) => {
                    LogRollerMeta::validate_template(filename_str.as_ref(), rotation_age)?;
                }
                _ => {
                    return Err(LogRollerError::InvalidFilenameTemplate(
                        "filename templates are only supported with age-based rotation".into(),
                    ));
                }
            }
        }
        self.meta.is_filename_template = is_template;

        // Pre-compile the file-name regex so directory scans never rebuild it.
        self.meta.file_pattern = Some(LogRollerMeta::build_file_pattern(
            filename_str.as_ref(),
            &self.meta.rotation,
            &self.meta.suffix,
            &self.meta.compression,
            self.meta.is_filename_template,
        )?);

        let curr_file_path = self.meta.get_curr_log_path();
        let next_pending_sequence =
            LogRollerState::get_next_pending_sequence(&self.meta.directory, &self.meta.filename);

        // Error checking for invalid compression level.
        #[cfg(feature = "xz")]
        if let Some(Compression::XZ(level)) = self.meta.compression {
            if level > 9 {
                return Err(LogRollerError::InvalidXZCompressionLevel { level });
            }
        }
        Ok(LogRoller {
            meta: self.meta.to_owned(),
            state: LogRollerState {
                next_pending_sequence,
                next_age_based_time: self.meta.next_time(
                    self.meta.now(),
                    match &self.meta.rotation {
                        Rotation::AgeBased(rotation_age) => rotation_age.to_owned(),
                        _ => RotationAge::Daily,
                    },
                )?,
                curr_file_path: curr_file_path.to_owned(),
                curr_file_size_bytes: LogRollerState::get_curr_size_based_file_size(
                    &self.meta.directory.join(&self.meta.filename),
                ),
            },
            writer: self.meta.create_log_file(&curr_file_path)?,
            worker_tx: None,
            worker_handle: None,
        })
    }
}

#[allow(clippy::io_other_error)]
impl io::Write for LogRoller {
    /// Write a buffer to the log file, rotating first if the pending data would
    /// exceed the threshold.
    ///
    /// Rotation happens **before** the write so messages land in the correct
    /// file for their time period. Post-rotation work (compression, archive
    /// shifting, pruning) is always handed off to the background worker so it
    /// never blocks the write path.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(new_log_path) = Self::should_rollover(&self.meta, &self.state, buf.len() as u64) {
            let old_log_path = self.state.curr_file_path.clone();
            let rotated_log_path = match &self.meta.rotation {
                Rotation::SizeBased(_) => {
                    let pending_log_path = self.meta.get_pending_log_path(self.state.next_pending_sequence);
                    self.state.next_pending_sequence += 1;
                    pending_log_path
                }
                Rotation::AgeBased(_) => new_log_path.to_owned(),
            };
            let rotated_path = self
                .meta
                .refresh_writer(&mut self.writer, old_log_path, rotated_log_path)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;

            self.post_rotation(rotated_path);

            match &self.meta.rotation {
                Rotation::SizeBased(_) => {
                    self.state.curr_file_size_bytes = 0;
                    self.state.curr_file_path = self.meta.get_curr_log_path();
                }
                Rotation::AgeBased(rotation_age) => {
                    self.state.curr_file_size_bytes = 0;
                    self.state.curr_file_path = new_log_path;
                    self.state.next_age_based_time = self
                        .meta
                        .next_time(self.meta.now(), rotation_age.to_owned())
                        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
                }
            }
        }

        self.writer.write_all(buf)?;
        self.state.curr_file_size_bytes = self.state.curr_file_size_bytes.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    /// Flush data to the kernel page cache.
    ///
    /// When `graceful_shutdown` is enabled, this also drains the post-rotation
    /// worker so all compression and pruning completes before returning.
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()?;

        if self.meta.graceful_shutdown {
            // Drop the sender so the worker sees Disconnected, drains any
            // remaining jobs, and exits.
            self.worker_tx = None;
            Self::join_worker(&mut self.worker_handle, "graceful flush");
        }
        Ok(())
    }
}

impl Drop for LogRoller {
    fn drop(&mut self) {
        // Flush to the kernel page cache. The kernel will write it to disk on
        // its normal schedule (~30 s). Only kernel panic or power loss can
        // defeat the page cache — a process crash is safe.
        if let Err(e) = self.writer.flush() {
            eprintln!("LogRoller: failed to flush writer on drop: {e}");
        }

        // Signal the worker to drain and exit.
        self.worker_tx = None;

        if self.meta.graceful_shutdown {
            Self::join_worker(&mut self.worker_handle, "drop");
        }
    }
}

#[cfg(feature = "tracing")]
#[deprecated(
    since = "0.1.9",
    note = "Use LogRoller directly as an appender with tracing_appender::non_blocking"
)]
type _TracingFeatureIsDeprecated = ();
