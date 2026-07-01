//! Demonstrates custom log filename templates using chrono strftime tokens.
//!
//! When the filename contains `%Y`, `%m`, `%d`, `%H`, or `%M`, logroller
//! treats it as a format string and renders the rotation-boundary timestamp
//! directly into the filename.  When the filename has *no* `%` tokens, the
//! date is appended to the filename instead.
//!
//! Run with:
//!   cargo run --example template_filename
use {
    logroller::{LogRollerBuilder, Rotation, RotationAge},
    std::io::Write,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Minutely logs: myapp 2026-06-29 19:09.log, myapp 2026-06-29 19:10.log, ...
    let mut logger = LogRollerBuilder::new("./logs", "myapp %Y-%m-%d %H:%M.log")
        .rotation(Rotation::AgeBased(RotationAge::Minutely))
        .max_keep_files(3)
        .build()?;

    writeln!(logger, "server started on 0.0.0.0:8080")?;
    writeln!(logger, "connected to database primary@pg.example.com:5432/app")?;
    writeln!(logger, "worker pool ready: 8 threads")?;
    writeln!(logger, r#"GET /api/users 200 12.3ms"#)?;
    writeln!(logger, r#"POST /api/orders 201 45.7ms"#)?;
    writeln!(logger, "WARN  request latency p99 exceeded 500ms threshold")?;
    writeln!(logger, "ERROR payment gateway timeout after 30s: order_id=ef42a1")?;

    logger.flush()?;

    Ok(())
}
