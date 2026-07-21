# Scripts and Generated Contracts

Scripts are grouped by responsibility:

- `scripts/check-*` validates a bounded repository or public contract.
- `scripts/generate-*` writes a deterministic checked-in or staged artifact.
- `scripts/run-*` executes a gate and records SHA-named evidence.
- `tests/shell/*_smoke.sh` proves positive and negative script behavior.
- `release/checks/` composes the bounded open-source readiness gate.

Run `bash -n scripts/*.sh release/*.sh release/checks/*.sh tests/shell/*.sh`
after shell changes. A generator that owns a checked-in file must offer a drift
check and fail when the generated result differs. Release scripts must refuse
dirty candidates by default and must not push, tag, upload, or publish unless
the protected publication workflow explicitly owns that mutation.
