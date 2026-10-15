//! Forwards TCP connections that arrive on a ZeroTier address to a port on
//! this machine, e.g. TheHome's web server. Traffic to the ZeroTier address
//! ends in the plugin's userspace stack, so without this nothing would reach
//! TheHome. Plain TCP, so WebSockets pass through too.
//!
//! One thread listens, and one thread per connection moves bytes both ways.
//! lwIP sockets must stay on one thread, so each connection polls the
//! ZeroTier side and reads the local side without blocking.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::zt::Socket;

const BUF: usize = 16 * 1024;
/// Longest wait between looks at an idle connection.
const IDLE_MS: i32 = 20;

pub struct Proxy {
    pub listen: String,
    pub target: SocketAddr,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
}

impl Proxy {
    /// Listen on `ip:port` in ZeroTier and forward to `target`.
    pub fn start(ip: &str, port: u16, target: SocketAddr) -> Result<Proxy, String> {
        let listener = Socket::listen(ip, port)?;
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let (s, a) = (stop.clone(), active.clone());
        let listen = if ip.contains(':') { format!("[{ip}]:{port}") } else { format!("{ip}:{port}") };
        let name = format!("zt-listen-{port}");
        std::thread::Builder::new()
            .name(name)
            .spawn(move || accept_loop(listener, target, s, a))
            .map_err(|e| e.to_string())?;
        Ok(Proxy { listen, target, stop, active })
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Stop listening and close open connections.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: Socket, target: SocketAddr, stop: Arc<AtomicBool>, active: Arc<AtomicUsize>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.readable(250) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(e) => {
                crate::warn!("ZeroTier-lytteren stoppede: {e}");
                break;
            }
        }
        let (conn, remote) = match listener.accept() {
            Ok(c) => c,
            Err(e) => {
                crate::warn!("{e}");
                continue;
            }
        };
        let local = match TcpStream::connect_timeout(&target, Duration::from_secs(5)) {
            Ok(l) => l,
            Err(e) => {
                crate::warn!("{remote} kunne ikke viderestilles til {target}: {e}");
                continue;
            }
        };
        let (s, a) = (stop.clone(), active.clone());
        a.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new().name("zt-conn".into()).spawn(move || {
            pipe(conn, local, &s);
            a.fetch_sub(1, Ordering::Relaxed);
        });
        if let Err(e) = spawned {
            active.fetch_sub(1, Ordering::Relaxed);
            crate::warn!("{e}");
        }
    }
}

/// Move bytes both ways until both sides have finished or `stop` is set.
fn pipe(zt: Socket, local: TcpStream, stop: &AtomicBool) {
    let _ = local.set_nodelay(true);
    if local.set_nonblocking(true).is_err() {
        return;
    }
    let mut local = local;
    let mut buf = vec![0u8; BUF];
    let (mut zt_done, mut local_done) = (false, false);
    let mut wait = 0;
    while !(zt_done && local_done) && !stop.load(Ordering::Relaxed) {
        let mut moved = false;

        if !zt_done {
            match zt.readable(wait) {
                Ok(true) => match zt.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        zt_done = true;
                        let _ = local.shutdown(Shutdown::Write);
                    }
                    Ok(n) => {
                        if write_local(&mut local, &buf[..n], stop).is_err() {
                            return;
                        }
                        moved = true;
                    }
                },
                Ok(false) => {}
                Err(_) => return,
            }
        } else if wait > 0 {
            std::thread::sleep(Duration::from_millis(wait as u64));
        }

        if !local_done {
            match local.read(&mut buf) {
                Ok(0) => {
                    local_done = true;
                    zt.shutdown_write();
                }
                Ok(n) => {
                    if zt.write_all(&buf[..n]).is_err() {
                        return;
                    }
                    moved = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }

        wait = if moved { 0 } else { (wait * 2).clamp(1, IDLE_MS) };
    }
}

fn write_local(local: &mut TcpStream, mut data: &[u8], stop: &AtomicBool) -> std::io::Result<()> {
    while !data.is_empty() {
        match local.write(data) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                if stop.load(Ordering::Relaxed) {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
