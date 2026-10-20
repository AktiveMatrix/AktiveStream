//! SSH tunnel for reaching a host off-LAN. Shells out to the system `ssh` so it reuses the user's
//! keys, agent and config (BatchMode, so no interactive prompt). TCP only: this covers the RFB
//! path; the native path is QUIC/UDP, which SSH `-L` cannot forward, so for off-LAN native use a
//! flat network like Tailscale and connect to the Tailscale address directly.

use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::time::Duration;

/// A running `ssh -L` forward; killed when dropped.
pub struct Tunnel {
    pub local_port: u16,
    child: Child,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Opens `ssh -N -L 127.0.0.1:<free>:<target_host>:<target_port> <dest>` and waits for the forward
/// to accept connections. `spec` is `[user@]host[:ssh_port]`. Returns the tunnel to keep alive.
pub fn open(spec: &str, target_host: &str, target_port: u16) -> Result<Tunnel, String> {
    let (dest, ssh_port) = split_ssh_port(spec);
    let local_port = free_local_port()?;

    let mut command = Command::new("ssh");
    command.args([
        "-N",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "BatchMode=yes",
        "-L",
        &format!("127.0.0.1:{local_port}:{target_host}:{target_port}"),
    ]);
    if let Some(port) = ssh_port {
        command.args(["-p", &port.to_string()]);
    }
    command.arg(dest);

    let child = command.spawn().map_err(|e| format!("spawn ssh: {e}"))?;
    let mut tunnel = Tunnel { local_port, child };
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", local_port)).is_ok() {
            eprintln!("ssh tunnel up: 127.0.0.1:{local_port} -> {target_host}:{target_port} via {dest}");
            return Ok(tunnel);
        }
        if let Ok(Some(status)) = tunnel.child.try_wait() {
            return Err(format!("ssh exited before the forward came up ({status})"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("ssh tunnel did not come up within 10s".into())
}

/// Binds an ephemeral local port, then frees it for ssh to bind.
fn free_local_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}

/// Splits a trailing `:<port>` (the ssh port) off `[user@]host[:port]`.
fn split_ssh_port(spec: &str) -> (&str, Option<u16>) {
    if let Some((dest, port)) = spec.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (dest, Some(port));
    }
    (spec, None)
}

#[cfg(test)]
mod tests {
    use super::split_ssh_port;

    #[test]
    fn splits_optional_ssh_port() {
        assert_eq!(split_ssh_port("user@jump"), ("user@jump", None));
        assert_eq!(split_ssh_port("user@jump:2222"), ("user@jump", Some(2222)));
        assert_eq!(split_ssh_port("user@10.0.0.1"), ("user@10.0.0.1", None));
        assert_eq!(split_ssh_port("user@10.0.0.1:22"), ("user@10.0.0.1", Some(22)));
    }
}
