# Codex Continuation Notes

This repository was refactored from a single hard-coded NBD path into a generic block-device-based PoC with a memory-backed backend.

## What Changed

### New shared backend contract

- Added a new crate: `crates/block-device`
- Core types now live in `crates/block-device/src/lib.rs`
- Public pieces:
  - `DeviceGeometry`
  - `DeviceInfo`
  - `BuiltDevice<D>`
  - `Durability`
  - `BlockDevice`
- `BuiltDevice<D>` is the builder output shape expected by `nbd::server::NbdServer::mount(...)`
- The device builder pattern requested in-session is implemented by returning the initialized device plus `block_size` and `block_count` only from the final `build()` step

### New memory-backed PoC device

- Added a new crate: `crates/device-mem`
- Main implementation: `crates/device-mem/src/lib.rs`
- Builder: `MemoryDeviceBuilder`
- Current builder options:
  - `block_size(u32)`
  - `block_count(u64)`
  - `read_only(bool)`
  - `initial_data(Vec<u8>)`
- Final step:
  - `build() -> io::Result<BuiltDevice<MemoryDevice>>`
- Current backend behavior:
  - supports `read_at`
  - supports `write_at`
  - supports `flush`
  - advertises `supports_flush = true`
  - advertises `supports_fua = true`
  - does not implement trim/write-zeroes
- The backend now validates direct read/write ranges itself and returns `InvalidInput` instead of panicking

### NBD crate refactor

- Renamed the kernel ioctl wrapper from `crates/nbd/src/device.rs` to `crates/nbd/src/kernel_device.rs`
- `crates/nbd/src/server.rs` is now generic over `D: BlockDevice`
- `NbdServer::mount(...)` now takes `BuiltDevice<D>` instead of raw block geometry
- The server validates that `BuiltDevice` geometry matches `device.info()`
- NBD driver flags are now derived from `DeviceInfo` instead of using a fixed default

### Protocol changes

- `crates/nbd/src/proto.rs` no longer relies on the previous packed request layout helper
- Request parsing is explicit and currently supports:
  - `Read`
  - `Write`
  - `Disc`
  - `Flush`
  - `Trim`
  - `WriteZeroes`
- Fixed the previous `FUA` / `NO_HOLE` flag swap bug
- Added simple reply encoding via `SimpleReply`
- Added `driver_flags_from_device_info(...)`

### Rootless transport/session layer

- Added `crates/nbd/src/session.rs`
- `NbdSession<D, S>` runs over a generic async byte stream instead of requiring `/dev/nbdX`
- Current internal model:
  - single reader
  - concurrent workers
  - single reply writer
- Bounded concurrency:
  - default maximum in-flight requests is `16`
  - permit is acquired before a write payload is buffered
- Request behavior:
  - write payloads are fully read before dispatch
  - replies may be sent out of order
  - only the writer task touches the stream write side
- Durability behavior:
  - `FUA` maps to `Durability::Durable`
  - `FLUSH` waits for all earlier mutating requests before executing `device.flush()`
- Mutating request ordering is tracked with a monotonically increasing sequence in `MutationTracker`

### Binary wiring

- `oxycrypt/src/main.rs` now mounts a memory-backed backend instead of the previous placeholder path
- Current CLI shape:
  - `oxycrypt mount /dev/nbdN --block-size <u32> --block-count <u64>`
- The `target` path is parsed to extract the kernel NBD index

## Tests Added

### `device-mem`

- `crates/device-mem/tests/memory_device.rs`
- Coverage:
  - builder returns initialized device and geometry
  - read/write roundtrip
  - mismatched `initial_data` length is rejected

### `nbd`

- protocol tests in `crates/nbd/src/proto.rs`
- session tests in `crates/nbd/src/session.rs`
- coverage includes:
  - `FUA` / `NO_HOLE` parsing
  - driver-flag derivation from `DeviceInfo`
  - request bounds validation
  - out-of-order replies
  - `FLUSH` waiting for earlier writes
  - `FUA` reply only after durable write completion
  - shutdown while work is in flight

### privileged integration test

- Added ignored test: `crates/nbd/tests/mount.rs`
- This mounts a real `/dev/nbd0` backed by the memory device
- It is intentionally ignored because it requires:
  - root
  - a free `/dev/nbd0`

## Validation State

Validated successfully with:

```bash
cargo check
cargo test --config 'target.x86_64-unknown-linux-gnu.runner = "env"' --workspace
```

## Important Repo Gotcha

`.cargo/config.toml` contains:

```toml
[target.x86_64-unknown-linux-gnu]
runner = "sudo --preserve-env=RUST_LOG"
```

Because of that, plain `cargo test` will try to run tests through `sudo` and fail in a non-interactive session.

For unattended validation, override the runner in the command:

```bash
cargo test --config 'target.x86_64-unknown-linux-gnu.runner = "env"' --workspace
```

## Known Limitations / Follow-Up Ideas

- The PoC backend is memory-backed only
- `supports_trim` and `supports_write_zeroes` remain disabled
- `can_multi_conn` remains disabled
- `FUA` and `flush()` are modeled correctly at the protocol/session layer, but the current memory backend treats flush as a no-op because there is no external persistence boundary
- There is not yet a richer user-facing configuration path for choosing backend type or seeding large images
- There is not yet a dedicated doc/README for the new backend architecture beyond this handoff note

## Best Next Places To Continue

1. Add a real persistent backend crate once the semantics are settled further
2. Expose builder/backend selection more cleanly from the CLI
3. Decide whether `NbdSession::with_max_in_flight(...)` should be surfaced in server configuration
4. Expand privileged NBD integration coverage beyond the ignored smoke test
5. Add higher-level documentation for the crate split and the backend contract
