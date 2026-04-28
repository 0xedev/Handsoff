use handoff_cli::setup::ca_install_command;
use std::path::Path;

#[test]
fn ca_install_command_returns_some_on_supported_os() {
    let cert = Path::new("/tmp/test-ca.pem");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(ca_install_command(cert).is_some());
}

#[test]
fn ca_install_command_returns_sudo_prefix() {
    let cert = Path::new("/tmp/test-ca.pem");
    #[cfg(target_os = "macos")]
    {
        let cmd = ca_install_command(cert).unwrap();
        assert_eq!(cmd[0], "sudo");
        assert!(cmd.contains(&"security".to_string()));
    }
    #[cfg(target_os = "linux")]
    {
        let cmd = ca_install_command(cert).unwrap();
        assert_eq!(cmd[0], "sudo");
    }
}
