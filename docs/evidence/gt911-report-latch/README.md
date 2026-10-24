# GT911 unread report custody

Base: `6c069f272ad8e9918ddf29623e213cd6817562f8`.

The [GOODIX GT911 Programming Guide Rev.10](https://www.lcd-module.de/fileadmin/eng/pdf/zubehoer/GT911_Programming_Guide_Rev.10.pdf), section5 p28, specifies that unread ready coordinates remain unchanged; the host clears status0x814e after reading. Section3.3 defines the ready/count/key status. The model previously replaced coordinates on each host input and ignored the clear write.

The GT911-only latch holds one complete ready report and one latest pending state. Status, Home and coordinates remain stable across reads until an explicit zero write to0x814e. Clearing promotes pending latest state; intermediate ordinary movements are not a FIFO. Other touch-controller behavior is unchanged.

The existing host cancellation mechanism supplies the invalid single-contact point(480,800). This is a modeled host-abort sentinel, not a physical sensor report. A bounded sticky invalid report preserves abort custody behind an unread valid report, followed by the latest post-abort state. It prevents a queued release from turning cancellation into a valid tap. This exceptional barrier is not a generic touch-history queue.

Tests use the actual I2cDevice address/read/write path and seven-byte packet shape used by the SDK: unread DOWN787 then MOVE757; updates between packet bytes; Home changes; exact zero-status acknowledgement; pending latest/release; and unread valid→invalid abort→UP. Nonzero0x814e and zero0x814f do not acknowledge. Existing CST820 controls remain applicable. The host input ABI models one contact only; no multi-contact or stable tracking-ID capability is added.

This change does not model scan cadence or INT pulse timing. A physical scan need not capture the first external pointer location. CPU opportunities and host admission traces do not prove a controller read or firmware gesture recognition. The same ordinary firmware image must be manually qualified separately.

## Intentional panel golden change

The unchanged public panel demo polls GT911 during its existing SID script. A bounded test-only I2cDevice observer reproduces each arm's exact uninstrumented instruction total. The nineteen scripted DOWN/MOVE states and four UP states now yield nineteen contact reports and four release reports; the other150polls return not-ready. The old model returned twenty-one contact reports plus152always-ready empty reports. Both arms perform173status reads and173zero clears. This changes actual firmware polling work and interrupt interleaving. Console and audio remain identical. Only `panel-sid.report.txt` and `panel-sid.insns` are intentionally updated; [numeric evidence](panel-polling.json) retains both arms. This public-demo proof does not qualify physical touch scanning.
