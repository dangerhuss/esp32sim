# X3 SPI SD and normal C3 SDK persistence (EX233)

Base `57df8eae0952d110bb38eb85331b2e3642e6fc03`; model code
`ad956093` (SPI implementation `ba7fbaf2`). This is the SPI-mode counterpart to
EX232's native S3 SDMMC path, not a replacement of that bus or a firmware stub.
The shared finite `SdCard` media/CSD/CID is used by a bounded byte-level SD SPI
transport. The C3 SPI2 controller uses shared GDMA walkers restricted to data
SRAM for both descriptors and payloads. An X3 card-only fixture checks SPI2
CLK8/MOSI10/MISO7/CS12, rail13 and the parked panel CS21. Explicit `--sd-save`
preserves final bytes to a new regular file; the input is never overwritten.

Protocol facts: SD Physical Layer Simplified v5.10 section7; C3 register/trigger
facts: ESP-IDF6.1-dev `fff9895c82d744c7237be8847347bdd1b07c6643`,
`components/soc/esp32c3/register/soc/{spi_reg,gdma_reg}.h`. The normal driver is
`components/esp_driver_sdspi/src/sdspi_host.c` at that revision. No silicon
calibration or inherited S3 timing claims are made.

[Persistence receipt](persistence.json) pins the ordinary ESP-IDF application,
source digest, images/config, model and actual results. The firmware source is
[dangerhuss/crusty](https://github.com/dangerhuss/crusty/tree/68a4cdfc19b2364469461cd5f4b7f68c178894f5/tools/idf-scheduler-poc).
It uses the real SDK SPI/GDMA/FatFs path and no emulator-only firmware flags.
A disposable8MiB FAT card receives a deterministic1MiB file; a separate process
boots the identical image/config and reads/checks it, then independent host FAT
extraction checks the payload. C3 write/read work:1,390,172/620,335 simulatedus;
maximum foreground gap100,001us under the150,000us smoke bound. A32KiB internal
bulk buffer and4KiB internal DMA buffer reclaim their full36,872-byte allocator
charge. SDK allocations acquired during I/O remain separately observed. S3's
native SDMMC/initialized8MiB PSRAM and both chips' CPU regressions pass.

Negative runs are retained outside Git. The first SDK command revealed the
mandatory Ncr byte between command and R1, which the initial transport omitted;
the model now supplies it. The first storage verifier also incorrectly required
the internal pool to ignore SDK allocations during I/O. Immediate before/after
buffer-release observations now check exact owned reclamation. Tests additionally
reject wrong routes/power/select, invalid command/data CRC, out-of-range blocks,
truncated writes, bad descriptor ownership/destinations/short chains and occupied
save outputs. TX descriptor failure cannot clock partial input as a complete
card command; RX descriptor errors cannot publish successful EOF.

Checks: native and wasm32 Clippy with warnings denied; release workspace tests
including ignored cases (1,034 pass,0fail/ignored,22 developer-input cases filtered);
actual WASM smoke manifests hello,c3-hello,c6-hello,c6-energy-scan,c6-contiki,
c6-contiki-net,c6-rpl-net,panel; evidence privacy pattern check. Crusty's required
local gate and36 hostile Python controls also pass. These are supporting model
and safety checks, not runtime Prometheus or physical-device qualification.

## Reproduce

Build this emulator normally with `cargo build --release --bins`. In the guest
repository at the pinned source, provide installed SDK/compiler/Python/ROM paths:

```sh
IDF_TARGET=esp32c3 make idf-poc-build BUILD_ROOT="$C3_BUILD"
IDF_TARGET=esp32c3 make idf-poc-run-storage BUILD_ROOT="$C3_BUILD" OUT="$FRESH_OUT"
```

The guest README documents `IDF_PATH`, `IDF_PYTHON`, `IDF_TOOLS_BIN`, `ESP32SIM`
and `ROM`. Every OUT is fresh and outside Git; fixture creation refuses existing
outputs. Reproduce the normal S3 CPU/storage regression with the matching S3
compiler/ROM and a separate build root. Use the commands in CONTRIBUTING.md for
native/WASM checks. The full private/local debug captures are not committed.

## Unused-path comparison

[Off-path receipt](offpath.json): three alternating pairs against the base,
normal release binaries,30 simulated seconds per hello demo, identical
instruction/cycle/exception/interrupt totals and console hashes. Median user CPU:
S3 0.26→0.26s (0.0%); C3 2.41→2.46s (+2.07%); C6 3.16→3.17s (+0.32%). Short
samples and host noise limit interpretation; no speedup or exact-zero overhead
claim. No S3 core changes, so Pocket Tank is outside this change's required lane.

This is functional SPI/card/DMA persistence. Physical card timing, electrical
behavior, caches/contention, sudden power loss, full X3 display, browser card
attachment, connected C3 workloads/retained Prom and physical X3 remain unqualified.
