//! Start the embedded node, print its address and follow one network.
//! cargo run --example node -- <dir> <network id>

use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("dir");
    let net = u64::from_str_radix(&args.next().expect("network id"), 16).expect("hex");
    thehome_zerotier::zt::start(std::path::Path::new(&dir)).unwrap();
    let t = Instant::now();
    while !thehome_zerotier::zt::online() && t.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("online={} address={:?} after {:?}", thehome_zerotier::zt::online(), thehome_zerotier::zt::address(), t.elapsed());
    thehome_zerotier::zt::join(net).unwrap();
    for _ in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        println!("{:?}", thehome_zerotier::zt::network(net));
    }
}
