use {
    logroller::{Compression, LogRollerBuilder, Rotation, RotationAge},
    std::io::Write,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create an error logger with custom suffix and compression
    let mut error_logger = LogRollerBuilder::new("./logs", "app.log")
        .rotation(Rotation::AgeBased(RotationAge::Hourly))
        .suffix("error".to_string()) // Will create files like: app.log.2025-04-01-19.error
        .compression(Compression::Gzip) // Compress old log files to save space
        .max_keep_files(24) // Keep one day of hourly error logs
        .build()?;

    // Simulate some error logging
    for error_code in &[500, 502, 503, 504] {
        writeln!(
            error_logger,
            "Error {error_code}: Server encountered an internal error, please try again later",
        )?;
        writeln!(error_logger, "Stack trace for error {error_code}:")?;
        writeln!(error_logger, "  at processRequest (server.rs:42)")?;
        writeln!(error_logger, "  at handleConnection (network.rs:121)")?;
        writeln!(error_logger, "  at main (main.rs:15)")?;
        writeln!(error_logger, "--------------------")?;
    }

    Ok(())
}
