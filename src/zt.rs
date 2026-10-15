//! ZeroTier inside the plugin, through libzt (vendor/libzt, built by
//! build.rs). libzt runs one node per process with its own userspace TCP/IP
//! stack (lwIP), so it needs no tun device, root or extra capabilities.
//!
//! The node keeps its identity in a folder, so its address survives restarts.
//! All calls are blocking C calls; callers run them on blocking threads.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_ushort, c_void};

// lwIP is built without full duplex: use a socket from one thread at a time
unsafe impl Send for Socket {}
use std::path::Path;
use std::sync::Mutex;

mod ffi {
    use super::*;

    extern "C" {
        pub fn zts_init_from_storage(path: *const c_char) -> c_int;
        pub fn zts_node_start() -> c_int;
        pub fn zts_node_stop() -> c_int;
        pub fn zts_node_is_online() -> c_int;
        pub fn zts_node_get_id() -> u64;
        pub fn zts_net_join(net_id: u64) -> c_int;
        pub fn zts_net_leave(net_id: u64) -> c_int;
        pub fn zts_net_get_status(net_id: u64) -> c_int;
        pub fn zts_net_get_type(net_id: u64) -> c_int;
        pub fn zts_net_get_name(net_id: u64, dst: *mut c_char, len: c_uint) -> c_int;
        pub fn zts_net_get_mac_str(net_id: u64, dst: *mut c_char, len: c_uint) -> c_int;
        pub fn zts_addr_get_str(net_id: u64, family: c_uint, dst: *mut c_char, len: c_uint) -> c_int;
        pub fn zts_socket(family: c_int, kind: c_int, protocol: c_int) -> c_int;
        pub fn zts_bind(fd: c_int, ipstr: *const c_char, port: c_ushort) -> c_int;
        pub fn zts_listen(fd: c_int, backlog: c_int) -> c_int;
        pub fn zts_accept(fd: c_int, remote: *mut c_char, len: c_int, port: *mut c_ushort) -> c_int;
        pub fn zts_read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
        pub fn zts_write(fd: c_int, buf: *const c_void, len: usize) -> isize;
        pub fn zts_close(fd: c_int) -> c_int;
        pub fn zts_bsd_shutdown(fd: c_int, how: c_int) -> c_int;
        pub fn zts_set_no_delay(fd: c_int, enabled: c_int) -> c_int;
        pub fn zts_bsd_poll(fds: *mut PollFd, nfds: c_uint, timeout: c_int) -> c_int;
        pub fn zts_tcp_client(remote_ipstr: *const c_char, remote_port: c_ushort) -> c_int;
    }

    #[repr(C)]
    pub struct PollFd {
        pub fd: c_int,
        pub events: i16,
        pub revents: i16,
    }
}

const AF_INET: c_int = 0x0002;
const AF_INET6: c_int = 0x000a;
const SOCK_STREAM: c_int = 0x0001;
const SHUT_WR: c_int = 0x1;
const POLLIN: i16 = 0x001;
const ADDR_LEN: usize = 46;

/// Network status codes from libzt.
pub fn status_name(code: i32) -> &'static str {
    match code {
        0 => "REQUESTING_CONFIGURATION",
        1 => "OK",
        2 => "ACCESS_DENIED",
        3 => "NOT_FOUND",
        4 => "PORT_ERROR",
        5 => "CLIENT_TOO_OLD",
        _ => "UNKNOWN",
    }
}

fn string(buf: &[u8]) -> String {
    CStr::from_bytes_until_nul(buf).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// What libzt knows about a joined network.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetInfo {
    pub status: String,
    pub name: String,
    pub private: bool,
    pub mac: String,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

static STARTED: Mutex<Option<String>> = Mutex::new(None);

/// Start the node with its identity in `dir`. Only the first call starts it;
/// later calls must use the same folder.
pub fn start(dir: &Path) -> Result<(), String> {
    let mut started = STARTED.lock().unwrap();
    let dir = dir.to_string_lossy().into_owned();
    if let Some(d) = started.as_ref() {
        return if *d == dir { Ok(()) } else { Err(format!("ZeroTier kører allerede med {d}")) };
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
    let c = CString::new(dir.clone()).map_err(|e| e.to_string())?;
    let rc = unsafe { ffi::zts_init_from_storage(c.as_ptr()) };
    if rc != 0 {
        return Err(format!("zts_init_from_storage: {rc}"));
    }
    let rc = unsafe { ffi::zts_node_start() };
    if rc != 0 {
        return Err(format!("zts_node_start: {rc}"));
    }
    *started = Some(dir);
    Ok(())
}

/// Stop the node, e.g. before the process exits.
pub fn stop() {
    let mut started = STARTED.lock().unwrap();
    if started.take().is_some() {
        unsafe { ffi::zts_node_stop() };
    }
}

pub fn is_started() -> bool {
    STARTED.lock().unwrap().is_some()
}

pub fn online() -> bool {
    is_started() && unsafe { ffi::zts_node_is_online() } == 1
}

/// The node's address, ten hex digits, once the identity is loaded.
pub fn address() -> Option<String> {
    if !is_started() {
        return None;
    }
    let id = unsafe { ffi::zts_node_get_id() };
    (id != 0).then(|| format!("{id:010x}"))
}

pub fn join(net: u64) -> Result<(), String> {
    match unsafe { ffi::zts_net_join(net) } {
        0 => Ok(()),
        rc => Err(format!("zts_net_join: {rc}")),
    }
}

pub fn leave(net: u64) -> Result<(), String> {
    match unsafe { ffi::zts_net_leave(net) } {
        0 => Ok(()),
        rc => Err(format!("zts_net_leave: {rc}")),
    }
}

/// `None` until the node has heard from the network (or after leaving).
pub fn network(net: u64) -> Option<NetInfo> {
    if !is_started() {
        return None;
    }
    let code = unsafe { ffi::zts_net_get_status(net) };
    if code < 0 {
        return None;
    }
    let mut name = [0u8; 128];
    let mut mac = [0u8; 24];
    let mut v4 = [0u8; ADDR_LEN + 1];
    let mut v6 = [0u8; ADDR_LEN + 1];
    unsafe {
        ffi::zts_net_get_name(net, name.as_mut_ptr() as *mut c_char, name.len() as c_uint);
        ffi::zts_net_get_mac_str(net, mac.as_mut_ptr() as *mut c_char, mac.len() as c_uint);
    }
    let ok4 = unsafe { ffi::zts_addr_get_str(net, AF_INET as c_uint, v4.as_mut_ptr() as *mut c_char, v4.len() as c_uint) } == 0;
    let ok6 = unsafe { ffi::zts_addr_get_str(net, AF_INET6 as c_uint, v6.as_mut_ptr() as *mut c_char, v6.len() as c_uint) } == 0;
    Some(NetInfo {
        status: status_name(code).to_string(),
        name: string(&name),
        private: unsafe { ffi::zts_net_get_type(net) } == 0,
        mac: string(&mac),
        ipv4: Some(string(&v4)).filter(|s| ok4 && !s.is_empty()),
        ipv6: Some(string(&v6)).filter(|s| ok6 && !s.is_empty()),
    })
}

/// A TCP socket in ZeroTier's userspace stack.
pub struct Socket(c_int);

impl Socket {
    /// Listen on `ip:port`, where `ip` is one of the node's ZeroTier
    /// addresses.
    pub fn listen(ip: &str, port: u16) -> Result<Socket, String> {
        let family = if ip.contains(':') { AF_INET6 } else { AF_INET };
        let fd = unsafe { ffi::zts_socket(family, SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(format!("zts_socket: {fd}"));
        }
        let s = Socket(fd);
        let c = CString::new(ip).map_err(|e| e.to_string())?;
        let rc = unsafe { ffi::zts_bind(fd, c.as_ptr(), port) };
        if rc < 0 {
            return Err(format!("zts_bind {ip}:{port}: {rc}"));
        }
        let rc = unsafe { ffi::zts_listen(fd, 16) };
        if rc < 0 {
            return Err(format!("zts_listen: {rc}"));
        }
        Ok(s)
    }

    /// Connect to `ip:port` over ZeroTier.
    pub fn connect(ip: &str, port: u16) -> Result<Socket, String> {
        let c = CString::new(ip).map_err(|e| e.to_string())?;
        let fd = unsafe { ffi::zts_tcp_client(c.as_ptr(), port) };
        if fd < 0 {
            return Err(format!("zts_tcp_client {ip}:{port}: {fd}"));
        }
        unsafe { ffi::zts_set_no_delay(fd, 1) };
        Ok(Socket(fd))
    }

    /// Wait up to `timeout_ms` for something to read (or the end of the
    /// stream, or an error).
    pub fn readable(&self, timeout_ms: i32) -> Result<bool, i32> {
        let mut p = ffi::PollFd { fd: self.0, events: POLLIN, revents: 0 };
        let rc = unsafe { ffi::zts_bsd_poll(&mut p, 1, timeout_ms) };
        if rc < 0 { Err(rc) } else { Ok(rc > 0 && p.revents != 0) }
    }

    /// The next connection and the remote address. Call it once `readable`
    /// says so, so it doesn't block.
    pub fn accept(&self) -> Result<(Socket, String), String> {
        let mut remote = [0u8; ADDR_LEN + 1];
        let mut port: c_ushort = 0;
        // libzt wants exactly ZTS_IP_MAX_STR_LEN here
        let fd = unsafe { ffi::zts_accept(self.0, remote.as_mut_ptr() as *mut c_char, ADDR_LEN as c_int, &mut port) };
        if fd < 0 {
            return Err(format!("zts_accept: {fd}"));
        }
        unsafe { ffi::zts_set_no_delay(fd, 1) };
        Ok((Socket(fd), format!("{}:{port}", string(&remote))))
    }

    /// Bytes read; 0 at the end of the stream.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, i32> {
        let n = unsafe { ffi::zts_read(self.0, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n < 0 { Err(n as i32) } else { Ok(n as usize) }
    }

    pub fn write_all(&self, mut buf: &[u8]) -> Result<(), i32> {
        while !buf.is_empty() {
            let n = unsafe { ffi::zts_write(self.0, buf.as_ptr() as *const c_void, buf.len()) };
            if n <= 0 {
                return Err(n as i32);
            }
            buf = &buf[n as usize..];
        }
        Ok(())
    }

    /// No more writes; the other side reads the end of the stream.
    pub fn shutdown_write(&self) {
        unsafe { ffi::zts_bsd_shutdown(self.0, SHUT_WR) };
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        unsafe { ffi::zts_close(self.0) };
    }
}
