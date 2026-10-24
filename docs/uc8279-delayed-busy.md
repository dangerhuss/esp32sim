# UC8279 refresh-start timing scenario

The opt-in S3 board alias `x4pro-delayed-busy` schedules BUSY assertion 2,000
microseconds after an accepted DRF (0x12), then uses the existing nominal refresh
duration (150 ms partial, 600 ms full). The ordinary `x4pro` board remains immediate.
The delay is synthetic and uncalibrated, not a measured or guaranteed silicon value.
Power-on/off timing, chip-select framing, planes, and optics are unchanged.

Accepted refresh custody begins at DRF, including the interval before BUSY asserts.
A command such as PTOUT (0x92) or OLD-plane synchronization (0x10) in that interval
is a protocol fault, just as during asserted BUSY. This observation does not render
or establish a physical stripe. Pixels publish only at the existing refresh end.
Reset cancels both scheduled edges. A jump across both edges preserves their order.

Maintainer reference: FreeInk commit 699370183fa3a0e33c9cb83a36f701bbb6022095,
`libs/display/FreeInkDisplay/src/driver/Uc8279X4Driver.cpp` displayStart/displayFinish, explicitly polls
for BUSY assertion after DRF before completion and OLD synchronization. CrossPoint's
FreeInk commit 32a265fa86498c014e13a5bc16133bb021cb620c has the same contract.
These are implementation references, not an exact UC8279/C hardware datasheet.
No minimum BUSY pulse or maximum start latency is claimed here.

For the existing two-device viewer, retain profile `x4pro` and all ordinary firmware
bytes. Its worker currently hardcodes the constructor board name: a reviewed private
worker derivative can replace only that constructor string with this alias; the
fixture's emulator.wasm selects this module. There is no existing config board key.
The comparison uses the same module and original `x4pro` constructor. Root operators
perform manual MCU-WASM interaction; this scenario provides no UI driver or firmware
telemetry and makes no physical-cause or hardware qualification claim.

`esp32sim_board_report(handle)` queues the existing X4 board report as one `emu`
outbox message and returns 0; null/non-X4 handles return 1 without a message.
This read-only export does not advance the guest. The observer-only
`esp32sim_reports` export does not include this controller report. A private worker
may project only the anchored numeric X4 fields; raw general reports are unnecessary.
