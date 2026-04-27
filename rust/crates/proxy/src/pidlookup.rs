//! Map a TCP peer (local addr + port) -> owning PID.
//!
//! Linux: parse `/proc/net/tcp[6]` for a row whose `local_address` matches
//! our peer, get the inode, then scan `/proc/<pid>/fd` for a symlink to
//! `socket:[<inode>]`.
//!
//! macOS: shell out to `lsof -i tcp -n -P -F pn`.

use std::net::SocketAddr;

pub fn lookup_pid(peer: SocketAddr) -> Option<i64> {
    #[cfg(target_os = "linux")]
    return linux::lookup(peer);
    #[cfg(target_os = "macos")]
    return macos::lookup(peer);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = peer;
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::net::SocketAddr;

    pub(super) fn hex_v4(addr: &SocketAddr) -> Option<String> {
        let SocketAddr::V4(a) = addr else { return None };
        let octets = a.ip().octets();
        // /proc/net/tcp uses little-endian for the IP
        let hex_ip = format!(
            "{:02X}{:02X}{:02X}{:02X}",
            octets[3], octets[2], octets[1], octets[0]
        );
        Some(format!("{hex_ip}:{:04X}", a.port()))
    }

    fn read_inode(target: &str) -> Option<String> {
        for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(body) = std::fs::read_to_string(path) else { continue };
            for line in body.lines().skip(1) {
                let parts: Vec<_> = line.split_whitespace().collect();
                if parts.len() < 10 {
                    continue;
                }
                if parts[1].eq_ignore_ascii_case(target) {
                    return Some(parts[9].to_string());
                }
            }
        }
        None
    }

    pub fn lookup(peer: SocketAddr) -> Option<i64> {
        let target = hex_v4(&peer)?;
        let inode = read_inode(&target)?;
        if inode == "0" {
            return None;
        }
        let needle = format!("socket:[{inode}]");
        for entry in std::fs::read_dir("/proc").ok()? {
            let Ok(entry) = entry else { continue };
            let pid_str = entry.file_name();
            let Some(pid_s) = pid_str.to_str() else { continue };
            let Ok(pid) = pid_s.parse::<i64>() else { continue };
            let fd_dir = format!("/proc/{pid}/fd");
            let Ok(fds) = std::fs::read_dir(&fd_dir) else { continue };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    if target.to_string_lossy() == needle {
                        return Some(pid);
                    }
                }
            }
        }
        None
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::net::SocketAddr;
    use std::process::Command;

    pub fn lookup(peer: SocketAddr) -> Option<i64> {
        let port = peer.port();
        let res = Command::new("lsof")
            .args(["-iTCP", &format!("-i:{port}"), "-n", "-P", "-F", "pn"])
            .output()
            .ok()?;
        if !res.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&res.stdout);
        let mut current_pid: Option<i64> = None;
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix('p') {
                current_pid = rest.parse::<i64>().ok();
            } else if line.starts_with('n') && current_pid.is_some() {
                let needle_a = format!("{}:{port}", peer.ip());
                let needle_b = format!("*:{port}");
                if line.contains(&needle_a) || line.contains(&needle_b) {
                    return current_pid;
                }
            }
        }
        current_pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn lookup_returns_some_or_none_without_panicking() {
        let _ = lookup_pid(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hex_v4_format() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let s = super::linux::hex_v4(&addr).unwrap();
        assert_eq!(s, "0100007F:1F90");
    }
}
