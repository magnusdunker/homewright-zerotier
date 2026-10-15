# libzt, vendored

Only the files the plugin's `build.rs` compiles, copied from:

| Path | Repository | Commit |
|---|---|---|
| `include/`, `src/`, `LICENSE.txt`, `README.md` | https://github.com/zerotier/libzt | a707ea6ae0910efdc1125d04758c411e2e9ea4f9 |
| `ext/ZeroTierOne/` (`node/`, `osdep/`, `include/`, `version.h`, `ext/libnatpmp`, `ext/miniupnpc`, `ext/prometheus-cpp-lite-1.0`) | https://github.com/zerotier/ZeroTierOne | c53c6bd9c320ea839c525972d8afba448a58606e |
| `ext/lwip/` (`src/`, `COPYING`) | https://github.com/lwip-tcpip/lwip | 32708c0a8b140efb545cc35101ee5fdeca6d6489 |
| `ext/lwip-contrib/ports/{unix,win32}` | https://github.com/lwip-tcpip/lwip-contrib | 4fd612c9c72dfcd1db6618bd59c1a17d9f5b55f8 |
| `ext/concurrentqueue/` | https://github.com/cameron314/concurrentqueue (via libzt) | as in libzt |

Changes:

- `ext/ZeroTierOne/osdep/Phy.hpp`: includes `../node/Metrics.hpp` on Windows too. Upstream only includes it in the Unix branch, which MSVC tolerates and GCC rejects.

Licenses: libzt and ZeroTierOne are Business Source License 1.1 (`LICENSE.txt`, `ext/ZeroTierOne/LICENSE.txt`), with Change Dates 2026-01-01 and 2025-01-01 after which these versions are under the Apache License 2.0. lwIP is BSD (`ext/lwip/COPYING`, and the headers in `ext/lwip-contrib`). concurrentqueue is BSD/Boost (`ext/concurrentqueue/LICENSE.md`).
