# EX234: unchanged C3 firmware WPA3-SAE H2E

This extends the connected SD/RNG/Ethernet WASM bindings in parent PR5, not the
SDK SD mechanics in EX232/EX233. The materially different question is whether
selecting the real SDK SAE path reproduces a pre-workload foreground deadline
failure observed on a physical single-core C3. The standard firmware image and
its compile flags stay unchanged. No foreground pause or global slowdown is injected.

SAE/group19 derives from spr-networks/oxwifid revision
`254818c6ee55937a20ac27ece62b2202baef3acf`, with attribution and adaptations in
[NOTICE](../../../esp-soc/src/wifi/sae/NOTICE). Independent anchors are the
Espressif IDF6.1 `components/wpa_supplicant/test_apps/main/test_sae.c` IEEE
802.11-2020 J.10 PWE/commit/KCK/PMK/PMKID vectors, and RFC4493 CMAC known answers.
For SAE AKM8, IDF6.1 `wpa_eapol_key_mic` uses AES-CMAC; SHA256 is the PTK KDF.
The upstream README's HMAC-SHA256 EAPOL description was not used as a protocol
oracle. Wrong password, malformed group/scalar/curve, reflected commit,
unverified confirm/MIC/replay, duplicate delivery and bounded pending responses
have deterministic controls. PMF key installation is covered; robust-management
and rekey behavior remain explicitly unsupported/refused.

The first actual guest run rejected association because C3 TX DMA's four-byte
FCS reserve was delivered as trailing SAE IEs. Its real 98-byte commit was a
valid group19 point followed by the reserve. The C3 adapter now removes the
trailer at an explicit DMA-to-radio interface, without matching a magic value
or relaxing the SAE parser. This is a model framing correction, not a firmware
workaround. Negative run/source patch and artifact remain retained privately.

## Retained runtime comparison

[retained-comparison.json](retained-comparison.json) records the exact firmware,
source and module hashes, measurements, controls and remaining limits. Every
runtime result below was checked against the guest/device's actual authenticated
Cloudflare upload in retained Prom, including exact packet payloads, acquisition
windows and all seven lifecycle phases. The host relays Ethernet, not telemetry
or terminated TLS. Each run retained all 84 packets and completed five seeded
logical jobs in order 5,4,2,3,1, with no remaining work-buffer ownership.

| Run | Association maximum FG heartbeat gap | Maximum tick gap during association | Workload maximum FG heartbeat gap | Aggregate FG budget |
| --- | ---: | ---: | ---: | --- |
| Physical X3, WPA3 | 262845 us | 1044 us | 100016 us | fail |
| WASM WPA3, first successful module | 233085 us | 1001 us | 100001 us | fail |
| WASM WPA3, final source module | 233598 us | 1001 us | 100001 us | fail |
| WASM WPA2 control, same final module | 100000 us | 1001 us | 100000 us | pass |

The heartbeat period is 100000 us and its maximum-gap budget is 150000 us;
these are scheduling observations, not measured display/input latency. The final
WASM WPA3 gap is 11.13% smaller than the single physical measurement. It reproduces
the pre-workload deadline failure through the real guest SAE path, but does not
establish calibrated timing or identify the particular vendor task/crypto routine.
No physical AP reconfiguration, firmware performance fix or device restoration was
performed. The physical foreground failure remains open.

## Reproduction and checks

The firmware comes from `dangerhuss/crusty` revision
`eeef3579f4454d9e58e25db597c458e56d64dfa2`, target `esp32c3`. Build the normal
`idf-poc-build` target with the installed IDF6.1 SDK and a separate C3 build root;
do not add emulator firmware defines. The private runtime card selects the normal
five-job profile, seed42, lifecycle observations and the actual receiver endpoint.

The Node host loads the standard C3 ROM, exact app/layout, ELF symbols and card
through the existing WASM APIs. It enables the real host entropy FIFO, steps
160000 cycles at a time at 160 MHz and exchanges bounded Ethernet frames with
`esp-soc/examples/ethernet_stdio`. Select only the virtual AP's authentication:
`ssid=NAME,psk=PASSWORD,auth=sae` or the same credentials with the default WPA2.
The firmware/card/ROM/final module are identical between those two controls.
The host JIT callback returns zero; no host-generated JIT blocks are used. Verify
the durable checkpoint and each acquired metric group against retained Prom.
Private credentials, profiles and original flash/card layout are required inputs,
not public fixtures; source-bound raw evidence is retained outside Git.

Local regression checks passed at runtime code revision
`d157bd31c3a25f11ec65b0b28018271222517a16`, with the C3 descriptor test-fixture
correction at `88cf98cfcb59c2e4b3565f57793f02367d7a740a`:

- Native Clippy over the workspace/all targets, warnings denied.
- WASM release Clippy with `jit-tests`, warnings denied.
- Release workspace suite, ignored tests included and `external_` filtered:
  1049 passed, zero failed/ignored; 22 external tests filtered.
- All eight documented WASM smoke scenarios, plus the shared-memory JIT handoff.

The initial full-suite failure exposed the C3 relay test fixture's missing FCS
reserve. The fixture now models the descriptor contract; full payload equality,
maximum Ethernet length, queue backpressure and exact drop assertions were kept.
The earlier zero-length TX-count conservation failure and its fix are also retained
in local check logs. No golden outputs were regenerated.

[unused-path-comparison.json](unused-path-comparison.json) compares three alternating
30-guest-second pairs against upstream main `63670bb1`. Console hashes, instructions,
cycles, exception/interrupt counts and stop reasons match exactly. Median host user
CPU times changed by -1.35% (S3 hello), -0.38% (C3 hello), -2.80% (C6 hello); these small
samples show no observed unused-path regression, not a claimed speed improvement.

All raw flash/SD inputs, profiles, per-acquisition Prom query results, loader/capture
logs and local runner paths stay outside Git. Public values here are curated numeric
observations and software identities. Omitted labels, boot identifiers and private
endpoints limit public independent re-query; hashes identify the retained originals.
