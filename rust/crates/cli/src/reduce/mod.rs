pub mod cargo_test;
pub mod git_diff;

use std::process::{Command, Stdio};
use anyhow::Result;

pub async fn run_reduce(args: &[String]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("Usage: handoff reduce <command> [args...]");
    }

    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let full_output = format!("{}{}", stdout, stderr);

    // Log original to ~/.handoff/logs/commands.log
    let log_dir = handoff_common::paths::home_dir().join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("commands.log");
    
    let header = format!("\n=== {} === {}\n", chrono::Utc::now().to_rfc3339(), args.join(" "));
    
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&log_path)?;
    
    use std::io::Write;
    file.write_all(header.as_bytes())?;
    file.write_all(full_output.as_bytes())?;

    // Apply reducer
    let reduced = match args[0].as_str() {
        "cargo" if args.get(1).map(|s| s == "test").unwrap_or(false) => {
            cargo_test::reduce(&full_output)
        }
        "git" if args.get(1).map(|s| s == "diff").unwrap_or(false) => {
            git_diff::reduce(&full_output)
        }
        _ => full_output.to_string(),
    };

    print!("{}", reduced);
    Ok(())
}
