use {
    logroller::{LogRollerBuilder, Rotation, RotationAge, TimeZone},
    std::io::Write,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut logger = LogRollerBuilder::new("./logs", "daily.log")
        .rotation(Rotation::AgeBased(RotationAge::Daily))
        .time_zone(TimeZone::UTC) // Use UTC for consistent timing across different regions
        .max_keep_files(7) // Keep one week of logs
        .build()?;

    // These logs will be rotated daily at UTC midnight
    writeln!(logger, "System startup - UTC timestamp will be used for rotation")?;
    writeln!(logger, "Configuration loaded successfully")?;
    writeln!(logger, "Server listening on port 8080")?;

    Ok(())
}
