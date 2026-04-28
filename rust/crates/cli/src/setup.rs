use std::path::Path;
use std::process::{Command, Stdio};
use anyhow::Result;

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
    println!("  handoff spawn claude");
    
    Ok(())
}
