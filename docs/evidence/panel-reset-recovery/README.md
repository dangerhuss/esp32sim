# Panel reset recovery

Base `cb06678ffeae69fe6071675fb82a5decd79ccaf7` used its cumulative protocol-fault counter as a permanent refresh refusal. A malformed transfer therefore poisoned X3/X4 refresh even after an actual controller reset and complete valid reinitialization.

Keep the cumulative counter unchanged and separately latch whether the current controller protocol epoch is invalid. Every existing fault path sets that latch, including X3 simultaneous SD/panel select. Only the existing actual reset GPIO assertion clears it. Reset still invalidates controller planes, pending refresh and the existing LUT/window state; valid plane/power/LUT/window checks remain mandatory. Visible glass is retained, not cleared by resetting diagnostic history.

Deterministic controls use the actual BoardModel GPIO/reset and SPI routing paths. Both old models fail fault→reset→complete initialization→refresh recovery. The repair permits completion only after BUSY, keeps cumulative faults visible, refuses complete writes without reset and refuses missing baseline planes after reset. X3 shared-select errors use the same refusal custody. These controls do not qualify physical reset timing or optical behavior. No firmware, register routing, deadline, cadence or network behavior changes.
