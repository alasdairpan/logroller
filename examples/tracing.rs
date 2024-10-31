use logroller::{Compression, LogRollerBuilder, Rotation, RotationAge, TimeZone};
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let appender = LogRollerBuilder::new("./logs", "tracing.log")
        .rotation(Rotation::AgeBased(RotationAge::Minutely))
        .max_keep_files(3)
        .time_zone(TimeZone::Local)
        .compression(Compression::Gzip)
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
