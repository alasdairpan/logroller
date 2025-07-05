use {
    logroller::{LogRoller, LogRollerBuilder, LogRollerError, Rotation, RotationSize},
    std::{io::Write, time::Instant},
};

const LOG_FOLDER: &str = "./logs/compression";

/// Very dependant on log pattern, On random pattern data might not be worth using compression
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let log_name = "gzip.log".to_string();
    let mut logger = LogRollerBuilder::new(LOG_FOLDER, &log_name)
        .rotation(Rotation::SizeBased(RotationSize::MB(1)))
        .max_keep_files(2)
        .compression(logroller::Compression::Gzip)
        .build()?;

    // Simulate writing logs that will trigger size-based rotation
    writing_log(&mut logger)?;
    let log_name = format!("{LOG_FOLDER}/{log_name}");

    std::fs::remove_file(&log_name)?;

    #[cfg(feature = "xz")]
    {
        for n in 0..=9 {
            let log_name = format!("xz_rate_{n}.log");
            let mut logger = LogRollerBuilder::new(LOG_FOLDER, &log_name)
                .rotation(Rotation::SizeBased(RotationSize::MB(1)))
                .max_keep_files(2)
                .compression(logroller::Compression::XZ(n))
                .build()?;

            // Simulate writing logs that will trigger size-based rotation
            writing_log(&mut logger)?;
            let log_name = format!("{LOG_FOLDER}/{log_name}");

            std::fs::remove_file(&log_name)?;
        }
    }

    #[cfg(not(feature = "xz"))]
    {
        println!("XZ compression examples skipped. Enable 'xz' feature to run XZ compression tests.");
    }
    println!("Done Compressing: {:?}", start.elapsed());
    println!("File | Compression percentage | Bytes");
    for i in std::fs::read_dir(LOG_FOLDER)?.flatten() {
        let size = get_curr_size_based_file_size(&i.path());
        println!(
            "{:?} : {:.2}% : {:?} Bytes",
            i.file_name(),
            size as f64 * 100.0 / (1024.0 * 1024.0),
            size
        );
    }

    Ok(())
}

fn get_curr_size_based_file_size(log_path: &std::path::Path) -> u64 {
    std::fs::metadata(log_path).map_or(0, |m| m.len())
}

/// This is log example, Compression rate will differ with log patterns.
fn writing_log(logger: &mut LogRoller) -> Result<(), LogRollerError> {
    for i in 1..=35_000 {
        writeln!(
            logger,
            "Log entry #{i}: This is a sample log message that will contribute to file size"
        )?;
    }
    logger.flush()?;
    Ok(())
}
