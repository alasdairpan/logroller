use chrono::{DateTime, FixedOffset, Local, NaiveTime, Timelike, Utc};
use flate2::write::GzEncoder;
use std::{
    fmt::Debug,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{PoisonError, RwLock, RwLockReadGuard},
};

#[derive(Debug, Clone)]
pub enum RotationSize {
    Bytes(u64),
    KB(u64),
    MB(u64),
    GB(u64),
}

impl RotationSize {
    fn bytes(&self) -> u64 {
        match self {
            RotationSize::Bytes(b) => *b,
            RotationSize::KB(kb) => kb * 1024,
            RotationSize::MB(mb) => mb * 1024 * 1024,
            RotationSize::GB(gb) => gb * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Compression {
    Gzip,
    Bzip2,
    LZ4,
    Zstd,
    XZ,
    Snappy,
}

#[derive(Debug, Clone)]
pub enum TimeZone {
    UTC,
    Local,
    Fix(FixedOffset),
}

#[derive(Clone, Debug)]
pub enum RotationAge {
    Minutely,
    Hourly,
    Daily,
}

#[derive(Clone)]
pub enum Rotation {
    SizeBased(RotationSize),
    AgeBased(RotationAge),
}

#[derive(Clone)]
struct LogRollerMeta {
    directory: PathBuf,
    filename: PathBuf,
    rotation: Rotation,
    time_zone: TimeZone,
    compression: Option<Compression>,
    // max_keep_files: Option<u64>,
    // max_compressed_files: Option<u64>,
}

struct LogRollerState {
    next_size_based_index: usize,
    next_age_based_time: DateTime<FixedOffset>,

    curr_file_path: PathBuf,
    curr_file_size_bytes: u64,
}

impl LogRollerState {
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

    fn get_curr_size_based_file_size(log_path: &Path) -> u64 {
        std::fs::metadata(log_path).map_or(0, |m| m.len())
    }
}

pub struct LogRoller {
    meta: LogRollerMeta,
    state: LogRollerState,
    writer: RwLock<fs::File>,
}

impl LogRoller {
    fn should_rollover(
        meta: &LogRollerMeta,
        state: &LogRollerState,
    ) -> Result<PathBuf, LogRollerError> {
        match &meta.rotation {
            Rotation::SizeBased(rotation_size) => {
                if state.curr_file_size_bytes >= rotation_size.bytes() {
                    return match &meta.rotation {
                        Rotation::AgeBased(_) => Err(LogRollerError::InvalidRotationType),
                        Rotation::SizeBased(_) => Ok(meta.directory.join(PathBuf::from(
                            format!(
                                "{}.{}",
                                meta.filename.as_path().to_string_lossy(),
                                state.next_size_based_index
                            )
                            .to_string(),
                        ))),
                    };
                }
            }
            Rotation::AgeBased(_) => {
                let now = meta.now();
                let next_time = state.next_age_based_time;
                if now >= next_time {
                    return match &meta.rotation {
                        Rotation::SizeBased(_) => Err(LogRollerError::InvalidRotationType),
                        Rotation::AgeBased(rotation_age) => {
                            let path_fn = |pattern: &str| -> PathBuf {
                                meta.directory.join(PathBuf::from(
                                    next_time
                                        .format(&format!(
                                            "{}.{pattern}",
                                            meta.filename.as_path().to_string_lossy()
                                        ))
                                        .to_string(),
                                ))
                            };
                            Ok(match rotation_age {
                                RotationAge::Minutely => path_fn("%Y-%m-%d-%H-%M"),
                                RotationAge::Hourly => path_fn("%Y-%m-%d-%H"),
                                RotationAge::Daily => path_fn("%Y-%m-%d"),
                            })
                        }
                    };
                }
            }
        }
        Err(LogRollerError::ShouldNotRotate)
    }
}

impl LogRollerMeta {
    fn now(&self) -> DateTime<FixedOffset> {
        let tz = match &self.time_zone {
            TimeZone::UTC => Utc::now().fixed_offset().offset().to_owned(),
            TimeZone::Local => Local::now().offset().to_owned(),
            TimeZone::Fix(offset) => offset.to_owned(),
        };
        Local::now().with_timezone(&tz)
    }

    #[allow(deprecated)]
    fn replace_time(
        &self,
        base_datetime: DateTime<FixedOffset>,
        time_to_replaced: NaiveTime,
    ) -> DateTime<FixedOffset> {
        DateTime::<FixedOffset>::from_local(
            base_datetime.date_naive().and_time(time_to_replaced),
            *base_datetime.offset(),
        )
    }

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
                    NaiveTime::from_hms_opt(d.hour(), d.minute(), 0)
                        .ok_or(LogRollerError::GetNaiveTimeFailed)?,
                ))
            }
            RotationAge::Hourly => {
                let d = base_datetime + chrono::Duration::hours(1);
                Ok(self.replace_time(
                    d,
                    NaiveTime::from_hms_opt(d.hour(), 0, 0)
                        .ok_or(LogRollerError::GetNaiveTimeFailed)?,
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

    fn create_log_file(&self, log_path: &Path) -> Result<fs::File, LogRollerError> {
        let mut open_options = fs::OpenOptions::new();
        open_options.append(true).create(true);

        let mut create_log_file_res = open_options.open(log_path);
        if create_log_file_res.is_err() {
            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| LogRollerError::CreateDirectoryFailed(err.to_string()))?;
                create_log_file_res = open_options.open(log_path);
            }
        }

        let log_file =
            create_log_file_res.map_err(|err| LogRollerError::CreateFileFailed(err.to_string()))?;

        Ok(log_file)
    }

    fn compress(
        compression: &Option<Compression>,
        log_path: &PathBuf,
    ) -> Result<(), LogRollerError> {
        let compression = match compression {
            Some(compression) => compression,
            None => {
                return Ok(());
            }
        };
        match compression {
            Compression::Gzip => {
                let infile = fs::File::open(log_path).map_err(LogRollerError::FileIOError)?;
                let reader = io::BufReader::new(infile);

                let outfile =
                    fs::File::create(PathBuf::from(format!("{}.gz", log_path.to_string_lossy())))
                        .map_err(LogRollerError::FileIOError)?;
                let writer = io::BufWriter::new(outfile);

                // Create a GzEncoder to compress the file
                let mut encoder = GzEncoder::new(writer, flate2::Compression::default());
                // Copy the content from the input file to the encoder (which compresses it)
                io::copy(&mut io::Read::take(reader, u64::MAX), &mut encoder)?;
                // Finish the compression
                encoder.finish()?;

                fs::remove_file(log_path).map_err(LogRollerError::FileIOError)?;
            }
            Compression::Bzip2
            | Compression::LZ4
            | Compression::Zstd
            | Compression::XZ
            | Compression::Snappy => {}
        }
        Ok(())
    }

    fn refresh_writer(
        &self,
        writer: &mut fs::File,
        olg_log_path: PathBuf,
        new_log_path: PathBuf,
    ) -> Result<(), LogRollerError> {
        let compression = self.compression.to_owned();
        match &self.rotation {
            Rotation::SizeBased(_) => {
                let curr_log_path = self.directory.join(&self.filename);
                std::fs::rename(&curr_log_path, &new_log_path)
                    .map_err(|_| LogRollerError::RenameFileError)?;

                match self.create_log_file(&curr_log_path) {
                    Ok(log_file) => {
                        if let Err(err) = writer.flush() {
                            eprintln!("Couldn't flush previous writer: {}", err);
                        }
                        *writer = log_file;

                        std::thread::spawn(move || {
                            if let Err(err) = Self::compress(&compression, &new_log_path) {
                                eprintln!("Couldn't compress log file: {}", err);
                            }
                        });
                    }
                    Err(err) => {
                        eprintln!("Couldn't create new log file: {}", err);
                    }
                }
            }
            Rotation::AgeBased(_) => match self.create_log_file(&new_log_path) {
                Ok(log_file) => {
                    if let Err(err) = writer.flush() {
                        eprintln!("Couldn't flush previous writer: {}", err);
                    }
                    *writer = log_file;

                    std::thread::spawn(move || {
                        if let Err(err) = Self::compress(&compression, &olg_log_path) {
                            eprintln!("Couldn't compress log file: {}", err);
                        }
                    });
                }
                Err(err) => {
                    eprintln!("Couldn't create new log file: {}", err);
                }
            },
        }
        Ok(())
    }
}

impl LogRollerMeta {
    fn new<P: AsRef<Path>>(directory: P, filename: P) -> Self {
        LogRollerMeta {
            directory: directory.as_ref().to_path_buf(),
            filename: filename.as_ref().to_path_buf(),
            rotation: Rotation::AgeBased(RotationAge::Daily),
            time_zone: TimeZone::Local,
            compression: None,
            // max_keep_files: None,
            // max_compressed_files: None,
        }
    }

    fn get_curr_log_path(&self) -> PathBuf {
        match &self.rotation {
            Rotation::SizeBased(_) => self.directory.join(self.filename.as_path()),
            Rotation::AgeBased(rotation_age) => {
                let now = self.now();
                let path_fn = |pattern: &str| -> PathBuf {
                    self.directory.join(PathBuf::from(
                        now.format(&format!(
                            "{}.{pattern}",
                            self.filename.as_path().to_string_lossy()
                        ))
                        .to_string(),
                    ))
                };
                match rotation_age {
                    RotationAge::Minutely => path_fn("%Y-%m-%d-%H-%M"),
                    RotationAge::Hourly => path_fn("%Y-%m-%d-%H"),
                    RotationAge::Daily => path_fn("%Y-%m-%d"),
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogRollerError {
    #[error("Failed to create directory: {0}")]
    CreateDirectoryFailed(String),
    #[error("Failed to create file: {0}")]
    CreateFileFailed(String),
    #[error("Failed to get native time")]
    GetNaiveTimeFailed,
    #[error("Invalid rotation type")]
    InvalidRotationType,
    #[error("Failed to get next file path")]
    GetNextFilePathError,
    #[error("Failed to rename file")]
    RenameFileError,
    #[error("File IO error: {0}")]
    FileIOError(#[from] std::io::Error),
    #[error("Should not rotate right now")]
    ShouldNotRotate,
}

pub struct LogRollerBuilder {
    meta: LogRollerMeta,
}

impl LogRollerBuilder {
    pub fn new<P: AsRef<Path>>(directory: P, filename: P) -> Self {
        LogRollerBuilder {
            meta: LogRollerMeta::new(directory, filename),
        }
    }

    pub fn time_zone(self, time_zone: TimeZone) -> Self {
        Self {
            meta: LogRollerMeta {
                time_zone,
                ..self.meta
            },
        }
    }

    pub fn rotation(self, rotation: Rotation) -> Self {
        Self {
            meta: LogRollerMeta {
                rotation,
                ..self.meta
            },
        }
    }

    pub fn compression(self, compression: Compression) -> Self {
        Self {
            meta: LogRollerMeta {
                compression: Some(compression),
                ..self.meta
            },
        }
    }

    pub fn build(self) -> Result<LogRoller, LogRollerError> {
        let curr_file_path = self.meta.get_curr_log_path();
        Ok(LogRoller {
            meta: self.meta.to_owned(),
            state: LogRollerState {
                next_size_based_index: LogRollerState::get_next_size_based_index(
                    &self.meta.directory,
                    &self.meta.filename,
                ),
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

impl io::Write for LogRoller {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let writer = self
            .writer
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);

        let old_log_path = self.state.curr_file_path.to_owned();
        if let Ok(new_log_path) = Self::should_rollover(&self.meta, &self.state) {
            self.meta
                .refresh_writer(writer, old_log_path, new_log_path.to_owned())
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
            self.state.curr_file_path.clone_from(&new_log_path);

            match &self.meta.rotation {
                Rotation::SizeBased(_) => {
                    self.state.curr_file_size_bytes = 0;
                    self.state.next_size_based_index += 1;
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
        self.state.curr_file_size_bytes += buf.len() as u64;
        writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }
}

#[cfg(feature = "tracing")]
impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for LogRoller {
    type Writer = RollingWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let old_log_path = self.state.curr_file_path.to_owned();
        if let Ok(new_log_path) = Self::should_rollover(&self.meta, &self.state) {
            let _ = self
                .meta
                .refresh_writer(
                    &mut self.writer.write().unwrap_or_else(PoisonError::into_inner),
                    old_log_path,
                    new_log_path.to_owned(),
                )
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()));
        }
        RollingWriter(self.writer.read().unwrap_or_else(PoisonError::into_inner))
    }
}

#[cfg(feature = "tracing")]
pub struct RollingWriter<'a>(RwLockReadGuard<'a, fs::File>);
#[cfg(feature = "tracing")]
impl io::Write for RollingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}
