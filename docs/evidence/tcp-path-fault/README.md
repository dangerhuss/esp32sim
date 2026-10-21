# Reversible TCP path faults

Base: `94c58257ecec4bad170d7df9c6f480861a596cd9` (the fork's merged SDMMC model).
The normal-release candidate adds `tcp offline` / `tcp online` action-script
commands. No firmware hooks, SDK stubs or guest build options are involved.

Offline closes existing host TCP sockets and refuses new TCP traffic with resets.
The virtual AP, DHCP, DNS/resolver, UDP and SNTP remain intact. Reset generation
follows [RFC9293 section3.10.7.1](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.10.7.1),
including ACK sequence handling, SYN/FIN lengths and discarding incoming resets.
Restoration permits the guest to open fresh connections with its unchanged SDK.
Unsupported transports fail explicitly. It models a TCP-path interruption, not
RF loss, a Cloudflare process failure, HTTP status, cache timing or physical power.

Validation used stable Rust1.99.1; `rustup check` reported stable up to date.
The unrelated nightly toolchain was not changed. Commands completed:

```sh
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo clippy --offline --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$ROM_DIR" cargo test --offline --release --workspace -- --include-ignored --skip external_
cargo build --offline --release --bins
# Normal tools/wasm-rustflags.sh settings, cache target directory:
cargo build --offline --release --target wasm32-unknown-unknown -p esp32sim-wasm
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Directed controls check TCP reset sequence/checksums, malformed headers and reset
loop refusal; actual loopback stream closure, retained UDP/resolver state and
restored host connectivity; script parsing, scheduling and intact AP/DHCP state.
No golden output was regenerated. Missing developer-only inputs retain the
repository's `external_` exclusion, rather than being treated as passing coverage.

Three alternating base/candidate pairs per chip, public hello images, normal
matching mask ROM, `--chip s3|c3|c6 --boot rom --flash-mb4 --max-seconds30
--no-dump --net none`, measured `/usr/bin/time -p` user seconds. Instruction counts
and complete guest console bytes match for every pair; numeric samples and hashes
are in `hello-comparison.json`. Median base -> candidate:

| Chip | Base | Candidate |
| --- | ---: | ---: |
| s3 | 0.25 | 0.26 |
| c3 | 2.43 | 2.46 |
| c6 | 3.05 | 3.09 |

These short samples have coarse timing resolution and host noise; no speedup or
calibrated slowdown is claimed. The S3 execution core was unchanged, so Pocket
Tank was not measured. Actual private application/Cloudflare workload acceptance
is a separate retained-Prometheus gate in Crusty issue401; these model tests do
not substitute for it. Timer-group watchdog registers remain unsupported as
already documented in `docs/peripherals.md`; no absence of events proves health.
