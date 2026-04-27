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

/// Parse `lsof -F pn -iTCP -i:<port>` output and find the PID whose socket
/// has `peer` as its **local** endpoint. Pure function, exposed for testing
/// on every platform (the macOS lookup just feeds in `lsof`'s real stdout).
///
/// On a localhost MITM connection both ends use the same ephemeral port, so
/// we must match the row whose `n` field starts with `<peer>:<port>->...`
/// (the agent's outbound socket), not the row whose remote endpoint matches
/// (that's the proxy itself — the v0.4.0-alpha bug).
#[allow(dead_code)] // Only called from #[cfg(macos)] mod and tests.
pub(crate) fn parse_lsof_pn(stdout: &str, peer: SocketAddr) -> Option<i64> {
    let port = peer.port();
    let local_prefix = format!("{}:{port}->", peer.ip());
    let mut current_pid: Option<i64> = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current_pid = rest.parse::<i64>().ok();
        } else if let Some(addrs) = line.strip_prefix('n') {
            if addrs.starts_with(&local_prefix) {
                return current_pid;
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
mod macos {
    use super::parse_lsof_pn;
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
        parse_lsof_pn(&stdout, peer)
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

    #[test]
    fn lsof_picks_agent_not_proxy_for_localhost_mitm() {
        // Regression test for the v0.4.0-alpha bug: on macOS, lsof reports
        // BOTH the agent (local=60000) and our proxy (remote=60000) when
        // queried with -i:60000. We must return the agent's PID, not the
        // proxy's.
        let stdout = "p73197\nn127.0.0.1:8080->127.0.0.1:60000\np73243\nn127.0.0.1:60000->127.0.0.1:8080\n";
        let peer: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        assert_eq!(parse_lsof_pn(stdout, peer), Some(73243));
    }

    #[test]
    fn lsof_returns_none_when_no_local_match() {
        let stdout = "p100\nn127.0.0.1:8080->127.0.0.1:99999\n";
        let peer: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        assert_eq!(parse_lsof_pn(stdout, peer), None);
    }

    #[test]
    fn lsof_only_matches_local_side() {
        // The proxy's row appears first; we must skip it and pick the
        // agent's row that follows.
        let stdout = concat!(
            "p100\n",
            "n127.0.0.1:8080->127.0.0.1:54321\n", // proxy ← agent
            "p200\n",
            "n127.0.0.1:54321->127.0.0.1:8080\n", // agent → proxy
        );
        let peer: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        assert_eq!(parse_lsof_pn(stdout, peer), Some(200));
    }
}
