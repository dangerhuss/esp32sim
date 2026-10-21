# Native SDMMC and ESP-IDF persistence proof (EX232)

Base: `63670bb128cf0a0c019a2009be62cfe56378ce0f`. Builds on the
existing SD card/CPU-FIFO commits `51bb2bc7` and `5d45549d`; adds the normal
ESP-IDF 6.1 internal IDMAC path, checked internal DRAM descriptors/buffers,
card initialization responses, and explicit regular-file image attachment/save.
No guest hooks or host-filesystem replacement of the guest SDK driver.

The unchanged ESP-IDF application at
[dangerhuss/crusty tools/idf-scheduler-poc](https://github.com/dangerhuss/crusty/tree/codex/idf-scheduler-poc-20261020/tools/idf-scheduler-poc)
uses native slot1 1-bit SDMMC and FatFs. Normal SDK startup detects/tests 8 MiB
octal PSRAM. A 32 KiB PSRAM bulk buffer feeds a 4 KiB internal DMA buffer.
A new 8 MiB FAT16 fixture receives a deterministic 1 MiB file. Checked
sync/close/unmount, separate-process restart/read, and independent SDK host
extraction all succeeded; the read boot did not mutate the retained card.
CPU and actual authenticated HTTPS scenarios also passed with the same image.

## Exact identities and results

- Guest prototype source SHA-256: `8c22fc0530795d618bb24991fe4f83c6fa0dd536f03ffdded1f517b5598de68a`.
- Guest app SHA-256: `8d26ee4a53bdabce34d3e2cea341b8d583f3ab06084c5ba47abf94db04c1da25`.
- Guest sdkconfig SHA-256: `3c5742f4bc0f1d07c5b5a98b03ab094d9a5a33cade54db68b17b07b6450080d4`.
- Independently extracted payload SHA-256: `ba6d132c0240202135463b11af17ce914f3c39e8675e885c5faef108c0a41a56`.
- SDK memory test: OK; usable PSRAM 8,388,608 bytes. Bulk allocation charged
  32,772 bytes and fully recovered. Mount-owned SDK buffers also recovered at
  unmount; whole-operation PSRAM free count returned to 8,386,156 bytes.
- SD write/read work: 341,663 / 200,843 emulated microseconds. Maximum foreground
  gap across SD/CPU/HTTPS scenarios: 100,421 us, under the 150,000 us smoke limit.
- Native and wasm32 Clippy, warnings denied: pass. CI-policy workspace release
  tests: 1,017 passed, zero failed/ignored, 22 `external_` cases filtered.

The initial SDK run exposed incorrect APP_CMD bit placement, clock-update
completion interrupts and missing SCR/SSR/CID behavior in the inherited model;
those were corrected against the driver/protocol definitions. A first guest
verifier conflated mounted filesystem allocations with the application bulk
buffer. It rejected a correctly reclaimed run; separate lifetime checks fixed
that assumption, and all final guest runs were regenerated. Earlier failures
and full source/image/card/log snapshots remain locally retained; they are not
published here and no physical qualification depends on their availability.

## Off-path native regression comparison

`offpath.json` retains all 18 measurements, execution totals, run order and
console hashes. Both revisions execute the identical instruction/cycle totals
and print identical console bytes for each demo. Three alternating pairs,
30 emulated seconds, `/usr/bin/time -p` user CPU seconds:

| Demo | Base median | Candidate median | Difference |
| --- | ---: | ---: | ---: |
| S3 hello | 0.25 | 0.25 | 0.0% |
| C3 hello | 2.36 | 2.39 | +1.27% |
| C6 hello | 3.21 | 3.15 | -1.87% |

Short runs, three pairs and host noise do not establish a speedup. S3 core
execution was not changed; Pocket Tank was not required or measured. Source and
fixture commands use normal release builds, with no special timing flag:

```sh
cargo build --release --locked --bins
# Repeat in alternating revision order for each chip; select matching ROM.
/usr/bin/time -p target/release/esp32sim --boot rom --rom "$ROM_S3" \
  --flash-mb 4 --bootloader web/wasm/fw/public/hello-bootloader.bin \
  --ptable web/wasm/fw/public/hello-ptable.bin \
  --app web/wasm/fw/public/hello_world.bin --max-seconds 30 --no-dump --net none
# C3/C6: esp32sim-c3 / esp32sim-c6, c3- / c6- file prefixes and matching ROMs.
```

To repeat the guest storage proof, build this emulator revision, select it with
`ESP32SIM`, and follow the Crusty PoC README's `idf-poc-build` and
`idf-poc-run-storage OUT=<fresh absolute directory>` commands. No hardware is
needed; the SDK fixture generator and parser are used by that command.

## Limits

Functional synchronous command/data completion does not model card/PSRAM cache
latency, power rails, GPIO electrical routing, concurrent DMA contention or
sudden power loss. SD buffers are internal DRAM only in this IDMAC boundary;
the guest explicitly stages PSRAM data through internal memory. The browser
front end does not yet expose these SD image controls. These native runs do not
claim browser persistence, physical X4 calibration or retained Prometheus
acceptance. No external service configuration was changed for this proof.
