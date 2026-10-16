//! Builds libzt (ZeroTier as a library, with the lwIP userspace TCP/IP
//! stack) from vendor/libzt and links it statically. The source lists follow
//! libzt's CMakeLists.txt; see vendor/libzt/VENDORED.md.

use std::path::{Path, PathBuf};

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect();
    v.sort();
    v
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("vendor/libzt");
    println!("cargo:rerun-if-changed=vendor/libzt");
    println!("cargo:rerun-if-changed=build.rs");

    let target = std::env::var("TARGET").unwrap();
    let windows = target.contains("windows");
    let msvc = target.contains("msvc");
    let apple = target.contains("apple");

    let zto = root.join("ext/ZeroTierOne");
    // ZeroTier's version, shown on the node device
    let version_h = std::fs::read_to_string(zto.join("version.h")).unwrap();
    let part = |name: &str| {
        version_h
            .lines()
            .find_map(|l| l.strip_prefix(&format!("#define ZEROTIER_ONE_VERSION_{name} ")))
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| "0".into())
    };
    println!("cargo:rustc-env=ZT_CORE_VERSION={}.{}.{}", part("MAJOR"), part("MINOR"), part("REVISION"));
    let lwip = root.join("ext/lwip/src");
    let port = root.join(if windows { "ext/lwip-contrib/ports/win32" } else { "ext/lwip-contrib/ports/unix/port" });
    let includes = [
        zto.clone(),
        zto.join("include"),
        zto.join("node"),
        zto.join("osdep"),
        zto.join("ext/libnatpmp"),
        zto.join("ext/prometheus-cpp-lite-1.0/core/include"),
        zto.join("ext/prometheus-cpp-lite-1.0/simpleapi/include"),
        root.join("src"),
        root.join("include"),
        root.join("ext/concurrentqueue"),
        lwip.join("include"),
        port.join("include"),
    ];

    let common = |b: &mut cc::Build| {
        b.includes(&includes).warnings(false).extra_warnings(false);
        if windows {
            b.define("ADD_EXPORTS", "1").define("STATICLIB", None).define("MINIUPNP_STATICLIB", None).define("NOMINMAX", None);
            b.define("_CRT_SECURE_NO_WARNINGS", None);
        }
        // as CMake does for MSVC; MinGW defines WIN32 itself
        if msvc {
            b.define("WIN32", None).define("_WINDOWS", None);
        }
        if !msvc {
            b.flag_if_supported("-w");
        }
        // GCC's outline atomics live in libgcc, which Rust's musl link leaves out
        if target.starts_with("aarch64") && !apple && !msvc {
            b.flag_if_supported("-mno-outline-atomics");
        }
    };

    // lwIP
    let mut c = cc::Build::new();
    common(&mut c);
    c.cargo_metadata(false);
    c.define("LWIP_DBG_TYPES_ON", "0");
    // old C that newer GCC and Clang reject by default
    for f in ["-Wno-incompatible-pointer-types", "-Wno-int-conversion", "-Wno-implicit-function-declaration"] {
        c.flag_if_supported(f);
    }
    for dir in ["netif", "api", "core", "core/ipv4", "core/ipv6"] {
        for f in files(&lwip.join(dir), "c") {
            if !f.ends_with("slipif.c") {
                c.file(f);
            }
        }
    }
    c.file(port.join("sys_arch.c"));

    // libnatpmp and miniupnpc, which ZeroTier uses for port mapping
    let natpmp = zto.join("ext/libnatpmp");
    c.file(natpmp.join("natpmp.c")).file(natpmp.join("getgateway.c"));
    if windows {
        c.file(natpmp.join("wingettimeofday.c"));
    }
    let upnp = zto.join("ext/miniupnpc");
    for f in [
        "connecthostport.c",
        "igd_desc_parse.c",
        "minisoap.c",
        "minissdpc.c",
        "miniupnpc.c",
        "miniwget.c",
        "minixml.c",
        "portlistingparse.c",
        "receivedata.c",
        "upnpcommands.c",
        "upnpdev.c",
        "upnperrors.c",
        "upnpreplyparse.c",
    ] {
        c.file(upnp.join(f));
    }
    for (k, v) in [
        ("ZT_USE_MINIUPNPC", None),
        ("MINIUPNP_STATICLIB", None),
        ("MINIUPNPC_SET_SOCKET_TIMEOUT", None),
        ("MINIUPNPC_GET_SRC_ADDR", None),
        ("_BSD_SOURCE", None),
        ("_DEFAULT_SOURCE", None),
        ("MINIUPNPC_VERSION_STRING", Some("\"2.0\"")),
        ("UPNP_VERSION_STRING", Some("\"UPnP/1.1\"")),
        ("ENABLE_STRNATPMPERR", None),
        ("OS_STRING", Some("\"Darwin/15.0.0\"")),
    ] {
        c.define(k, v);
    }
    if apple {
        c.define("_DARWIN_C_SOURCE", None).define("MACOSX", None);
    }
    c.compile("zt_c");

    // ZeroTier core and libzt
    let mut cpp = cc::Build::new();
    common(&mut cpp);
    cpp.cpp(true).cargo_metadata(false);
    if msvc {
        cpp.flag("/EHsc").flag("/std:c++14").flag("/FIstdexcept");
    } else {
        cpp.flag("-std=c++11");
        cpp.flag_if_supported("-Wno-template-body");
        // headers that newer standard libraries no longer pull in for free
        cpp.flag("-include").flag("stdexcept");
    }
    // ZT_NO_PEER_METRICS: a peer's Prometheus counters can be left unset, and
    // the first packet with a bad MAC then crashes the node; nothing reads them
    for (k, v) in [("ZT_SDK", "1"), ("ZT_USE_MINIUPNPC", "1"), ("_USING_LWIP_DEFINITIONS_", "0"), ("OMIT_JSON_SUPPORT", "1"), ("ZTS_DISABLE_CENTRAL_API", "1"), ("ZT_NO_PEER_METRICS", "1")] {
        cpp.define(k, v);
    }
    for f in files(&zto.join("node"), "cpp") {
        cpp.file(f);
    }
    cpp.file(zto.join("osdep/OSUtils.cpp")).file(zto.join("osdep/PortMapper.cpp"));
    for f in files(&root.join("src"), "cpp") {
        cpp.file(f);
    }
    cpp.compile("zt_cpp");

    // the C++ part uses lwIP, so it goes first for linkers that read once
    println!("cargo:rustc-link-search=native={}", std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-lib=static=zt_cpp");
    println!("cargo:rustc-link-lib=static=zt_c");
    if apple {
        println!("cargo:rustc-link-lib=c++");
    } else if !msvc {
        println!("cargo:rustc-link-lib=stdc++");
    }
    if windows {
        for lib in ["ws2_32", "iphlpapi", "shlwapi", "shell32", "advapi32", "user32"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
    // libzt starts its own threads with the default stack, which on musl is
    // 128 KiB. ZeroTier keeps 64 KiB buffers and whole packets on the stack,
    // so a thread ran over its stack into the next one and the node crashed
    // somewhere else each time. musl takes a larger default from the
    // binary's PT_GNU_STACK size.
    if target.contains("musl") {
        println!("cargo:rustc-link-arg-bins=-Wl,-z,stack-size=4194304");
    }
}
