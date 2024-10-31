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
