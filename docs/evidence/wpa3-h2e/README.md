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

Runtime comparisons and final source/artifact/check identities follow in the
receipt. All raw flash/SD inputs, profiles, per-acquisition Prom query results,
loader/capture logs and local runner paths stay outside Git. Public values here
are curated numeric observations and software identities, not device identifiers.
