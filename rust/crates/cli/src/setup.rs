//! First-time setup and teardown for the handoff tool.
//!
//! `run_setup()` scaffolds project state, generates the CA certificate, and
//! installs it into the OS trust store so the MITM proxy can intercept HTTPS.
//! `run_teardown()` revokes the CA and cleans up system trust entries.

use anyhow::Result;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn ca_install_command(cert_path: &Path) -> Option<Vec<String>> {
    let p = cert_path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    return Some(vec![
        "sudo".into(),
        "security".into(),
        "add-trusted-cert".into(),
        "-d".into(),
        "-r".into(),
        "trustRoot".into(),
        "-k".into(),
        "/Library/Keychains/System.keychain".into(),
        p,
    ]);
    #[cfg(target_os = "linux")]
    return Some(vec![
        "sudo".into(),
        "cp".into(),
        p.clone(),
        "/usr/local/share/ca-certificates/handoff.crt".into(),
    ]);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return None;
}

pub async fn run_setup(path: Option<&str>) -> Result<()> {
    let project_root = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    println!("Setting up handoff in {}", project_root.display());

    handoff_context::init_project(&project_root)?;
    println!("  ✓ Scaffolded .handoff/");

    let _ca = handoff_proxy::ca::load_or_create()?;
    let cert_path = handoff_common::home_dir().join("ca").join("cert.pem");
    println!("  ✓ CA cert at {}", cert_path.display());

    if let Some(cmd) = ca_install_command(&cert_path) {
        println!("  Installing CA into system trust store (requires sudo)...");
        let status = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        match status {
            Ok(s) if s.success() => {
                #[cfg(target_os = "linux")]
                {
                    let _ = Command::new("sudo").arg("update-ca-certificates").status();
                }
                println!("  ✓ System trust configured");
            }
            _ => {
                println!("  ⚠ Sudo failed. Run manually:\n    {}", cmd.join(" "));
            }
        }
    } else {
        println!("  ℹ Unsupported OS for auto-trust. Install cert manually.");
    }

    println!("\nSetup complete! You can now run agents through the proxy:");
    println!("  handoff daemon start");
    println!("  handoff proxy start");
    println!("  handoff spawn claude");

    Ok(())
}

/// Remove the handoff CA from the system trust store and clean up state.
///
/// Does not delete `brain.md` or snapshots — only trust store and generated certs.
pub async fn run_teardown() -> Result<()> {
    println!("Removing handoff CA from system trust store...");

    #[cfg(target_os = "macos")]
    {
        let status = Command::new("sudo")
            .args([
                "security",
                "delete-certificate",
                "-c",
                "handoff",
                "/Library/Keychains/System.keychain",
            ])
            .status();
        match status {
            Ok(s) if s.success() => println!("  ✓ CA removed from macOS Keychain"),
            _ => println!(
                "  ⚠ Could not remove CA automatically. Open Keychain Access and delete 'handoff'."
            ),
        }
    }

    #[cfg(target_os = "linux")]
    {
        let cert_dest = "/usr/local/share/ca-certificates/handoff.crt";
        let _ = Command::new("sudo").args(["rm", "-f", cert_dest]).status();
        let _ = Command::new("sudo").arg("update-ca-certificates").status();
        println!("  ✓ CA removed from Linux trust store");
    }

    // Remove the generated cert/key
    let ca_dir = handoff_common::home_dir().join("ca");
    if ca_dir.exists() {
        std::fs::remove_dir_all(&ca_dir)?;
        println!("  ✓ Deleted {}", ca_dir.display());
    }

    println!("\nNote: brain.md and snapshots are preserved in .handoff/");
    println!("To fully remove all state: rm -rf ~/.handoff");
    Ok(())
}
