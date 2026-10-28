# Sparse SDHC fixtures

The optional `sparse-sd` WASM feature permits the S3 SDMMC model to use a
bounded sparse card while running the ordinary firmware. Its logical capacity
can represent a multi-gigabyte SDHC card without allocating a contiguous
multi-gigabyte buffer. This models block contents and SDHC geometry, not a
physical card's latency, wear, electrical behavior, or contents.

`esp32sim_sd_attach_sparse` accepts at most 64 MiB of caller-supplied bytes before
boot. The format is `SDS1`, then three little-endian u32 fields: card capacity
in 512-byte sectors, record count, and maximum funded sectors. Each following
record is a little-endian u32 sector number and 512 bytes. Sector numbers must
be strictly increasing and within the card; count cannot exceed the funded
limit, which cannot exceed 131072. Capacity must exceed 2 GiB and be representable
exactly by CSD v2. Omitted sectors read as zeros. Admission constructs a new
card before replacing the previous one, so invalid or unfunded fixtures do not
alter the attached card. The ABI is S3-only; C3 and other boards refuse it.

Writes reserve all sectors of the requested transfer before acknowledging the
command. If a transfer exceeds media bounds or cannot be funded, the controller
reports an error before data transfer and existing sector bytes are preserved.
A transfer is limited to 64 KiB; larger requests are refused. A direct card
command without the host's transfer-length hint reserves the whole modeled
multi-block stream. The sector limit includes initial and later written sectors;
it is a host memory bound, not free space on the guest FAT filesystem.

The existing `esp32sim_sd_len` and `esp32sim_sd_copy` complete-image ABI returns
unavailable for sparse cards. No sparse export or durable restart is provided.
Callers must treat the card as disposable and must not claim a full checkpoint.
Without `sparse-sd`, the existing dense model and WASM API remain the default.
