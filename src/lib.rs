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
        sync::{PoisonError, RwLock},
    },
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
/// * XZ - Highest compression ratio but slower processing
/// * Snappy - Very fast compression, developed by Google
#[derive(Debug, Clone)]
pub enum Compression {
    /// Gzip compression, which provides a good balance of compression ratio
    /// and speed. Compressed files will have the `.gz` extension.
    Gzip,
    // Bzip2,
    // LZ4,
    // Zstd,
    XZ,
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
            Compression::XZ => "xz",
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
}

/// State for the log roller.
/// This struct is used to keep track of the current state of the log roller.
struct LogRollerState {
    /// The next index for size-based rotation.
    next_size_based_index: usize,
    /// The next time for age-based rotation.
    next_age_based_time: DateTime<FixedOffset>,
    /// The current file path.
    curr_file_path: PathBuf,
    /// The current file size in bytes.
    curr_file_size_bytes: u64,
}

impl LogRollerState {
    /// Get the next size-based index for the log file.
    /// This function will scan the directory for existing log files with the
    /// same name and return the next index based on the existing files.
    /// If no existing files are found, the index will be set to 1.
    /// The index is used to create a new log file with the same name but a
    /// different index. For example, if the log file is `app.log`, the next
    /// log file will be `app.log.1`. If the log file is `app.log.1`, the
    /// next log file will be `app.log.2`, and so on. The index is
    /// incremented each time a new log file is created.
    /// # Arguments
    /// * `directory` - The directory where the log files are stored.
    /// * `filename` - The name of the log file.
    /// # Returns
    /// The next size-based index for the log file.
    fn get_next_size_based_index(directory: &PathBuf, filename: &Path) -> usize {
        let mut max_suffix = 0;
        if directory.is_dir() {
            if let Ok(files) = std::fs::read_dir(directory) {
                for file in files.flatten() {
                    if let Some(exist_file) = file.file_name().to_str() {
                        if exist_file.starts_with(&filename.to_string_lossy().to_string()) {
                            if let Some(suffix_str) =
                                exist_file.strip_prefix(&format!("{}.", filename.to_string_lossy()))
                            {
                                if let Ok(suffix) = suffix_str.parse::<usize>() {
                                    max_suffix = std::cmp::max(max_suffix, suffix);
                                }
                            }
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
    meta: LogRollerMeta,
    state: LogRollerState,
    writer: RwLock<fs::File>,
}

impl LogRoller {
    /// Check if the log file should be rolled over.
    /// This function will check if the log file should be rolled over based on
    /// the rotation type. If the log file should be rolled over, the
    /// function will return the path to the new log file. If the log file
    /// should not be rolled over, the function will return None.
    /// # Arguments
    /// * `meta` - The metadata for the log roller.
    /// * `state` - The state for the log roller.
    /// # Returns
    /// The path to the new log file if the log file should be rolled over,
    /// otherwise None.
    fn should_rollover(meta: &LogRollerMeta, state: &LogRollerState) -> Option<PathBuf> {
        match &meta.rotation {
            Rotation::SizeBased(rotation_size) => {
                if state.curr_file_size_bytes >= rotation_size.bytes() {
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

    /// Process old log files.
    fn process_old_logs(&self, log_path: &PathBuf) -> Result<(), LogRollerError> {
        self.compress(log_path)?;
        let all_log_files = Self::list_all_files(
            &self.directory,
            self.filename.as_path().as_os_str().to_string_lossy().as_ref(),
            &self.rotation,
            &self.suffix,
            &self.compression,
        )?;

        // Remove old log files if necessary
        if let Some(max_keep_files) = self.max_keep_files {
            match &self.rotation {
                Rotation::SizeBased(_) => {
                    for file in all_log_files {
                        if let Some(index) = file
                            .path()
                            .file_name()
                            .and_then(|s| s.to_str())
                            .and_then(|s| {
                                let mut parts = s.split('.').collect::<Vec<&str>>();
                                if self.compression.is_some() {
                                    // Remove the compression extension
                                    parts.pop();
                                }
                                parts.last().cloned()
                            })
                            .and_then(|s| s.parse::<usize>().ok())
                        {
                            if index >= max_keep_files as usize {
                                let path = file.path();
                                if let Err(err) = fs::remove_file(&path) {
                                    eprintln!("Failed to remove old log file '{}': {}", path.display(), err);
                                }
                            }
                        }
                    }
                }
                Rotation::AgeBased(_) => {
                    if all_log_files.len() > max_keep_files as usize {
                        for file in all_log_files.iter().take(all_log_files.len() - max_keep_files as usize) {
                            let path = file.path();
                            if let Err(err) = fs::remove_file(&path) {
                                eprintln!("Failed to remove old log file '{}': {}", path.display(), err);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// List all log files in the directory.
    fn list_all_files(
        directory: &PathBuf,
        filename: &str,
        rotation: &Rotation,
        file_suffix: &Option<String>,
        compression: &Option<Compression>,
    ) -> Result<Vec<DirEntry>, LogRollerError> {
        let file_suffix = file_suffix.clone().map(|s| format!("(.{s})?")).unwrap_or_default();
        let compression_suffix = compression
            .clone()
            .map(|c| format!("(.{})?", c.get_extension()))
            .unwrap_or_default();
        let file_pattern = match rotation {
            Rotation::SizeBased(_) => Regex::new(&format!(r"^{filename}(\.\d+)?{file_suffix}{compression_suffix}$"))
                .map_err(|err| LogRollerError::InternalError(err.to_string()))?,
            Rotation::AgeBased(rotation_age) => {
                let pattern = match rotation_age {
                    RotationAge::Minutely => r"\d{4}-\d{2}-\d{2}-\d{2}-\d{2}",
                    RotationAge::Hourly => r"\d{4}-\d{2}-\d{2}-\d{2}",
                    RotationAge::Daily => r"\d{4}-\d{2}-\d{2}",
                };
                Regex::new(&format!(r"^{filename}\.{pattern}{file_suffix}{compression_suffix}$"))
                    .map_err(|err| LogRollerError::InternalError(err.to_string()))?
            }
        };

        let files = fs::read_dir(directory).map_err(|err| LogRollerError::InternalError(err.to_string()))?;

        let mut all_log_files = Vec::new();
        for file in files.flatten() {
            let metadata = file.metadata().map_err(LogRollerError::FileIOError)?;
            if !metadata.is_file() {
                continue;
            }
            if let Some(file_name) = file.file_name().to_str() {
                if file_pattern.is_match(file_name) {
                    all_log_files.push(file);
                }
            }
        }

        // Sort the log files by name
        all_log_files.sort_by_key(|f| f.file_name());

        Ok(all_log_files)
    }

    /// Compress the log file.
    fn compress(&self, log_path: &PathBuf) -> Result<(), LogRollerError> {
        let compression = match &self.compression {
            Some(compression) => compression,
            None => {
                return Ok(());
            }
        };
        let infile = fs::File::open(log_path).map_err(LogRollerError::FileIOError)?;
        let mut reader = io::BufReader::new(infile);

        let compressed_path = PathBuf::from(format!(
            "{}.{}",
            log_path.to_string_lossy(),
            compression.get_extension()
        ));
        let outfile = fs::File::create(&compressed_path).map_err(LogRollerError::FileIOError)?;
        let mut writer = io::BufWriter::new(outfile);

        match compression {
            Compression::Gzip => {
                let mut encoder = GzEncoder::new(writer, flate2::Compression::default());
                io::copy(&mut io::Read::take(reader, u64::MAX), &mut encoder)?;
                encoder.finish()?;
            }
            Compression::XZ => {
                lzma_rs::xz_compress(&mut reader, &mut writer)?;
                writer.flush()?;
            }
            /* Compression::Bzip2
             * | Compression::LZ4
             * | Compression::Zstd
             * | Compression::Snappy => {} */
        }
        // Ensures compressed file has correct permissions.
        self.set_permissions(&compressed_path)?;

        fs::remove_file(log_path).map_err(LogRollerError::FileIOError)?;
        Ok(())
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
    /// existing log files based on the rotation type.
    fn refresh_writer(
        &self,
        writer: &mut fs::File,
        old_log_path: PathBuf,
        new_log_path: PathBuf,
        next_size_based_index: usize,
        compression: &Option<Compression>,
    ) -> Result<(), LogRollerError> {
        let meta = self.to_owned();
        match &self.rotation {
            Rotation::SizeBased(_) => {
                let curr_log_path = self.directory.join(&self.filename);

                // 1. Rename the existing log files.
                // If target file exists, it will be overwritten
                for idx in (1..next_size_based_index).rev() {
                    // Rename original file
                    let source_file = self
                        .directory
                        .join(format!("{}.{}", self.filename.to_string_lossy(), idx));
                    let target_file = self
                        .directory
                        .join(format!("{}.{}", self.filename.to_string_lossy(), idx + 1));
                    if source_file.exists() {
                        std::fs::rename(&source_file, &target_file).map_err(|err| LogRollerError::RenameFileError {
                            from: source_file.clone(),
                            to: target_file.clone(),
                            error: err.to_string(),
                        })?;
                    }

                    // Rename compressed file
                    if let Some(compression) = &compression {
                        let compressed_source_file = self.directory.join(format!(
                            "{}.{}.{}",
                            self.filename.to_string_lossy(),
                            idx,
                            compression.get_extension()
                        ));
                        let compressed_target_file = self.directory.join(format!(
                            "{}.{}.{}",
                            self.filename.to_string_lossy(),
                            idx + 1,
                            compression.get_extension()
                        ));
                        if compressed_source_file.exists() {
                            std::fs::rename(&compressed_source_file, &compressed_target_file).map_err(|err| {
                                LogRollerError::RenameFileError {
                                    from: compressed_source_file.clone(),
                                    to: compressed_target_file.clone(),
                                    error: err.to_string(),
                                }
                            })?;
                        }
                    }
                }

                // 2. Rename the current log file
                std::fs::rename(&curr_log_path, &new_log_path).map_err(|err| LogRollerError::RenameFileError {
                    from: curr_log_path.clone(),
                    to: new_log_path.clone(),
                    error: err.to_string(),
                })?;

                // 3. Create a new log file and ensure proper cleanup on failure
                let new_log_file = match self.create_log_file(&curr_log_path) {
                    Ok(file) => file,
                    Err(err) => {
                        eprintln!("Failed to create new log file '{}': {}", curr_log_path.display(), err);
                        return Err(err);
                    }
                };

                // 4. Only update writer after successful file creation
                if let Err(err) = writer.flush() {
                    eprintln!("Failed to flush writer: {}", err);
                    return Err(LogRollerError::FileIOError(err));
                }
                *writer = new_log_file;

                // 5. Process old logs asynchronously
                std::thread::spawn(move || {
                    if let Err(err) = meta.process_old_logs(&new_log_path) {
                        eprintln!(
                            "Failed to process old log files for '{}': {}",
                            new_log_path.display(),
                            err
                        );
                    }
                });
            }
            Rotation::AgeBased(_) => {
                // 1. Create the new log file first
                let new_log_file = match self.create_log_file(&new_log_path) {
                    Ok(file) => file,
                    Err(err) => {
                        // Failed to create new file - keep using the existing one
                        eprintln!("Failed to create new log file '{}': {}", new_log_path.display(), err);
                        return Err(err);
                    }
                };

                // 2. Flush the existing writer before switching
                if let Err(err) = writer.flush() {
                    eprintln!("Failed to flush writer for '{}': {}", new_log_path.display(), err);
                    return Err(LogRollerError::FileIOError(err));
                }

                // 3. Update writer with new file only after successful flush
                *writer = new_log_file;

                // 4. Process old logs asynchronously
                std::thread::spawn(move || {
                    if let Err(err) = meta.process_old_logs(&old_log_path) {
                        eprintln!(
                            "Failed to process old log files for '{}': {}",
                            old_log_path.display(),
                            err
                        );
                    }
                });
            }
        }
        Ok(())
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
        }
    }

    /// Get the next log file path based on the rotation age.
    fn get_next_age_based_log_path(&self, rotation_age: &RotationAge, datetime: &DateTime<FixedOffset>) -> PathBuf {
        let path_fn = |pattern: &str| -> PathBuf {
            let mut tf = datetime
                .format(&format!("{}.{pattern}", self.filename.as_path().to_string_lossy()))
                .to_string();
            if let Some(suffix) = &self.suffix {
                tf = format!("{}.{}", tf, suffix);
            }
            self.directory.join(PathBuf::from(tf))
        };
        match rotation_age {
            RotationAge::Minutely => path_fn("%Y-%m-%d-%H-%M"),
            RotationAge::Hourly => path_fn("%Y-%m-%d-%H"),
            RotationAge::Daily => path_fn("%Y-%m-%d"),
        }
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
    #[error("Failed to set file permissions for '{path}': {error}")]
    SetFilePermissionsError { path: PathBuf, error: String },
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

    /// Build the log roller.
    pub fn build(self) -> Result<LogRoller, LogRollerError> {
        let curr_file_path = self.meta.get_curr_log_path();
        let mut next_size_based_index =
            LogRollerState::get_next_size_based_index(&self.meta.directory, &self.meta.filename);
        if let Some(max_keep_files) = self.meta.max_keep_files {
            next_size_based_index = next_size_based_index.min(max_keep_files as usize);
        }
        Ok(LogRoller {
            meta: self.meta.to_owned(),
            state: LogRollerState {
                next_size_based_index,
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
            writer: RwLock::new(self.meta.create_log_file(&curr_file_path)?),
        })
    }
}

#[allow(clippy::io_other_error)]
impl io::Write for LogRoller {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let writer = self.writer.get_mut().unwrap_or_else(PoisonError::into_inner);

        let old_log_path = self.state.curr_file_path.to_owned();
        let next_size_based_index = self.state.next_size_based_index;
        let compression = self.meta.compression.to_owned();

        // Write the log data to the current file
        let bytes = writer.write(buf)?;
        self.state.curr_file_size_bytes += bytes as u64;

        // Check if we need to rollover the log file
        if let Some(new_log_path) = Self::should_rollover(&self.meta, &self.state) {
            self.meta
                .refresh_writer(
                    writer,
                    old_log_path,
                    new_log_path.to_owned(),
                    next_size_based_index,
                    &compression,
                )
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
            self.state.curr_file_path.clone_from(&new_log_path);

            match &self.meta.rotation {
                Rotation::SizeBased(_) => {
                    self.state.curr_file_size_bytes = 0;
                    self.state.next_size_based_index += 1;
                    // If max_keep_files is set, the next_size_based_index should not exceed it
                    if let Some(max_keep_files) = self.meta.max_keep_files {
                        self.state.next_size_based_index =
                            self.state.next_size_based_index.min(max_keep_files as usize);
                    }
                }
                Rotation::AgeBased(rotation_age) => {
                    self.state.curr_file_size_bytes = 0;
                    self.state.next_age_based_time = self
                        .meta
                        .next_time(self.meta.now(), rotation_age.to_owned())
                        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
                }
            }
        }
        Ok(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.get_mut().unwrap_or_else(PoisonError::into_inner).flush()
    }
}

#[cfg(feature = "tracing")]
#[deprecated(
    since = "0.1.9",
    note = "Use LogRoller directly as an appender with tracing_appender::non_blocking"
)]
type _TracingFeatureIsDeprecated = ();
