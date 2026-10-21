# Explicit RNG input for live native runs

Base: `d838e57de8645af2589e52a216f05971e79bfe87`.
Implementation: `23907f62b2966c71caec7b0403a62efa2969d822`.

The offline deterministic RNG can repeat firmware boot identities across separate
runs. A receiver that owns immutable boot/sequence bytes must reject a later
conflicting packet. Changing firmware IDs, clearing receiver history or accepting
a conflicting ACK would hide the model defect.

`--rng-source /dev/urandom` explicitly supplies OS entropy at the real RNG register
boundary for native live-network runs. This adds no firmware flag, register-map
change, SDK stub or host uploader. The model performs no filesystem I/O; the
native frontend owns the input. Other ordinary files are deterministic fixtures.
Input read failure stops the emulator, never reverting to the offline stream.
Raw words are not logged. The input survives guest reset on all four chips.

Directed controls cover exact little-endian fixture words, exhaustion refusal,
missing/unsupported input, CLI opt-in, each chip's real RNG register and stream
continuation after reset. Register references are the installed ESP-IDF
`fff9895c82d744c7237be8847347bdd1b07c6643` headers named in the test.

Stable Rust 1.99.1 was current. `rustup check` exited 100 because an unrelated
nightly update exists; the stable entry explicitly reported up to date. No
unrelated toolchain was changed. Validation commands:

```sh
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo clippy --offline --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$ROM_DIR" cargo test --offline --release --workspace -- --include-ignored --skip external_
cargo build --offline --release --bins
# Standard tools/wasm-rustflags.sh production settings, output in a build cache:
cargo build --offline --release --target wasm32-unknown-unknown -p esp32sim-wasm
ESP32SIM_WASM="$WASM_MODULE" ESP32SIM_ROM_DIR="$ROM_DIR" node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Native/WASM Clippy, the full native suite (including ignored non-external tests),
production WASM scenarios and privacy checks passed. The first native-suite
invocation selected a directory without mask ROMs; its existing C3 BLE test failed
on the missing input. The complete suite then passed using the installed normal
mask-ROM directory. No golden outputs changed and no tests were weakened.

Default-off regression timings use three alternating base/candidate runs per
hello manifest, ordinary matching mask ROM, `--chip s3|c3|c6 --board none --boot
rom --flash-mb 4 --max-seconds 30 --no-dump --net none`. All guest core instruction
counts and complete console hashes must match. Numeric results are in
`hello-comparison.json`; these coarse samples support regression checking, not
calibrated hardware performance. The S3 execution core is unchanged, so Pocket
Tank is outside this change.

This input does not model radio-noise timing, RNG clock-enable gates or silicon
entropy quality. Default offline streams and browser behavior remain deterministic
unless a frontend explicitly provides a suitable entropy input. Browser regression
passes do not establish secure browser entropy. Actual authenticated application
uploads and retained-Prom acceptance remain a separate Crusty issue 401 gate.

The curated receipt omits private runtime credentials, packet captures, host paths,
process inventories and device/session identities. No secret entropy fixture is
retained here.
