use {
    logroller::{LogRollerBuilder, Rotation, RotationSize},
    std::io::Write,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut logger = LogRollerBuilder::new("./logs", "sized.log")
        .rotation(Rotation::SizeBased(RotationSize::MB(1))) // Rotate at 1MB
        .max_keep_files(5) // Keep only last 5 files
        .file_mode(0o640) // Set file permissions to: owner rw, group r, others none
        .build()?;

    // Simulate writing logs that will trigger size-based rotation
    for i in 1..=1000 {
        writeln!(
            logger,
            "Log entry #{}: This is a sample log message that will contribute to file size",
            i
        )?;
    }

    Ok(())
}
