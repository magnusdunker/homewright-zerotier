//! Proxy test over a ZeroTier ad-hoc network (no controller, no approval):
//!   adhoc serve <dir> <local port>      forwards ZeroTier port 8124 to 127.0.0.1:<local port>
//!   adhoc get <dir> <ipv6> <path>       fetches http://[ipv6]:8124<path> over ZeroTier

use std::time::{Duration, Instant};
use thehome_zerotier::{proxy::Proxy, zt};

// ad-hoc network allowing ports 8124-8124: ff + start + end + 000000
const NET: u64 = 0xff1fbc1fbc000000;

fn up(dir: &str) -> String {
    zt::start(std::path::Path::new(dir)).unwrap();
    let t = Instant::now();
    while !zt::online() {
        std::thread::sleep(Duration::from_millis(100));
    }
    zt::join(NET).unwrap();
    loop {
        if let Some(n) = zt::network(NET) {
            if let Some(ip) = n.ipv6.clone() {
                eprintln!("node {} online, {} {ip} after {:?}", zt::address().unwrap(), n.status, t.elapsed());
                return ip;
            }
        }
        assert!(t.elapsed() < Duration::from_secs(90), "no address");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a[0].as_str() {
        "serve" => {
            let ip = up(&a[1]);
            let p = Proxy::start(&ip, 8124, format!("127.0.0.1:{}", a[2]).parse().unwrap()).unwrap();
            println!("LISTEN {ip}");
            loop {
                std::thread::sleep(Duration::from_secs(1));
                eprintln!("active {}", p.active());
            }
        }
        "get" => {
            up(&a[1]);
            let t = Instant::now();
            let s = loop {
                match zt::Socket::connect(&a[2], 8124) {
                    Ok(s) => break s,
                    Err(e) if t.elapsed() < Duration::from_secs(60) => {
                        eprintln!("{e}");
                        std::thread::sleep(Duration::from_secs(1))
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            eprintln!("connected after {:?}", t.elapsed());
            s.write_all(format!("GET {} HTTP/1.0\r\nHost: x\r\n\r\n", a[3]).as_bytes()).unwrap();
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                }
            }
            use std::io::Write;
            let i = out.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).unwrap_or(0);
            eprintln!("{} bytes in {:?}", out.len() - i, t.elapsed());
            std::io::stdout().write_all(&out[i..]).unwrap();
            std::io::stdout().flush().unwrap();
            zt::stop();
            std::process::exit(0);
        }
        _ => panic!("serve or get"),
    }
}
