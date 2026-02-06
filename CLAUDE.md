# CLAUDE.md — PCPanel Pro

Native Linux daemon + CLI for PCPanel Pro USB audio mixer hardware. Rust workspace targeting Bazzite (Fedora atomic/immutable, SELinux enforcing, PipeWire + PulseAudio compat, KDE Plasma Wayland).

## Architecture

See `docs/architecture.md` for the full design document. Key constraints:

- **2-crate workspace**: `pcpaneld-core` (zero system deps), `pcpaneld` (daemon + CLI)
- **No shared mutable state.** All cross-boundary communication via typed tokio channels.
- **Engine owns all mutable state.** HID and PulseAudio run on `std::thread` (blocking APIs). Everything else is tokio tasks. The engine is a `tokio::select!` loop that receives from all subsystems.
- **Single device MVP.** No multi-device abstractions.

**Key modules in `pcpaneld`:** `engine.rs` (central `select!` loop, owns all mutable state, config file watching), `pulse.rs` (PA thread, `Rc<RefCell>`, single-threaded), `hid_thread.rs` (device lifecycle + reconnection), `signal.rs` (per-control jitter/debounce/curve pipeline), `tray.rs` (system tray via ksni), `mpris.rs` (media player control via D-Bus), `ipc_server.rs` (Unix socket server).

## Development Setup

Rust toolchain is on the host via rustup (`~/.cargo/bin`). Clippy and fmt work directly on the host (Rust-only analysis, no linker).

Building and testing need C libraries (`hidapi`, `libpulse`, etc.). On immutable distros (Bazzite/Fedora atomic), use a distrobox:

```bash
just setup    # creates pcpaneld-dev distrobox with all build deps
```

This runs:
```bash
distrobox create --name pcpaneld-dev --image registry.fedoraproject.org/fedora:43
distrobox enter pcpaneld-dev -- sudo dnf install -y \
    dbus-devel systemd-devel pulseaudio-libs-devel hidapi-devel gcc pkg-config
```

The resulting binary runs natively on the host (distrobox shares `$HOME` and `/run`). The only thing that touches the host OS is the udev rule (`sudo cp` to `/etc/udev/rules.d/`).

## Build & Check

```bash
just fmt      # apply rustfmt
just check    # clippy + fmt check (Rust-only analysis, no C linker needed)
just build    # compile (needs C dev libs — see Development Setup)
just test     # run tests (needs C dev libs)
just all      # check + build + test
just deny     # check advisories + licenses (run before PRs)
```

Raw commands if `just` is not available:
```bash
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
distrobox enter pcpaneld-dev -- cargo build --workspace
distrobox enter pcpaneld-dev -- cargo test --workspace
```

All four checks must pass with zero warnings before any change is considered complete.

## Workflow

- Branch off `main` for all changes. Use descriptive branch names (e.g., `upgrade-ksni-0.3`, `fix-slider-debounce`).
- One logical change per PR. Keep PRs focused and reviewable.
- `just all` must pass before opening a PR. Run `just deny` to check advisories/licenses.
- Use `gh pr create` to open PRs against `main`.

## Rust Standards

### Write Idiomatic Rust

- Use the type system to make illegal states unrepresentable. Prefer enums over stringly-typed fields. Prefer newtypes over raw primitives when the domain has distinct concepts (e.g., `ControlId`, `Volume`, `HwValue`).
- Use `#[must_use]` on functions where ignoring the return value is always a bug.
- Derive `Debug` on all public types. Derive `Clone`, `Copy`, `PartialEq`, `Eq` where semantically correct — don't derive traits speculatively.
- Prefer `impl Into<X>` / `AsRef` in function signatures only when the function genuinely benefits from multiple input types. Don't add generics for a single call site.
- Use iterators and combinators over manual loops when they're clearer. Don't chain 6 combinators when a `for` loop with an `if` is more readable.
- No `unwrap()` or `expect()` in library code (`pcpaneld-core`). In binaries, `expect()` is acceptable only during startup for things that are truly unrecoverable (e.g., "failed to bind IPC socket"). Everywhere else, propagate errors.
- No `clone()` to satisfy the borrow checker without understanding why. If you're cloning, justify it. `Arc` for shared ownership across threads/tasks. `Clone` for small value types. Neither as a band-aid.

### Error Handling

- `thiserror` in `pcpaneld-core` for typed, matchable errors (`HidError`, `ConfigError`, `IpcError`).
- `anyhow` in binary crates for top-level orchestration.
- Error types must carry enough context to diagnose the problem without a debugger. Include the operation that failed and relevant identifiers:
  ```rust
  // Good
  #[error("failed to write LED command to device {serial}: {source}")]

  // Bad
  #[error("write failed")]
  ```
- Never silently swallow errors. If an error is intentionally ignored, use `let _ =` with a comment explaining why.

### Dependencies

- Don't add dependencies without justification. Every dependency is an audit surface and a compile-time cost.

## Testing Philosophy

Tests exist to catch real bugs and protect against regressions. Every test should have a reason to exist — a scenario that could plausibly break and would matter if it did.

### What Makes a Good Test

A good test exercises a **meaningful behavior** with **realistic inputs** and asserts on **observable outcomes** that matter to users or callers.

### Test Anti-Patterns — Do Not Write These

- **Trivial identity tests**: `assert_eq!(ControlId::Knob(1), ControlId::Knob(1))` — this tests `PartialEq` derive, not your code.
- **Testing the compiler**: asserting that a constructor returns the values you passed in.
- **Snapshot tests of Debug output**: format strings are not contracts.
- **Tests that only exercise the happy path with toy inputs**: if a test uses `value = 42` and has no edge case coverage, it's incomplete.
- **Mocking so aggressively that the test only validates the mock wiring**: if you're asserting that a mock was called with specific args, you're testing the test, not the code. Assert on *outcomes*.
- **Tests that duplicate the implementation**: if the test contains the same math as the production code, it proves nothing. Test the *property* (e.g., monotonicity, round-trip, bounds) not the formula.

### Test Invariants, Not Formulas

Assert on behavioral guarantees that must hold across all valid inputs, not on specific computed values. Examples from this codebase:
- **Round-trip**: encode then decode returns the original value
- **Monotonicity**: increasing hardware input → non-decreasing volume output
- **Bounds**: output volume is always in [0.0, 1.0] regardless of input
- **Idempotency**: config reload with unchanged config changes nothing

If a test contains the same math as the production code, it proves nothing.

## Channel Conventions

- Position events (knob/slider analog values) are **replaceable** — only the latest value matters. Use `tokio::sync::watch` or accept that `try_send` on a bounded channel drops the newest.
- Button events (press/release) are **not droppable** — missing a release means stuck state. Use a reliable bounded channel.
- Don't use unbounded channels. Every channel has a documented bound and overflow strategy.

## Platform Notes

- **hidraw, not libusb.** The `hidapi` `linux-native` feature uses the kernel hidraw interface directly. No kernel driver detach needed.
- **Report ID behavior varies.** If the device uses Report ID 0, hidapi strips it. If non-zero, it's the first byte. Confirm by reading the HID report descriptor. This affects all byte offset calculations.
- **SELinux is enforcing on Bazzite.** If hidraw access is denied, check `ausearch -m avc -ts recent`. The udev `TAG+="uaccess"` rule should be sufficient but test this early.
- **PipeWire PA compat layer.** We use `libpulse-binding` which talks to PipeWire's PulseAudio compatibility interface. Subscribe callbacks and introspection work but test with PipeWire specifically, not assumptions from PulseAudio docs.
- **Flatpak apps.** On Bazzite, many apps are Flatpaks. Their `application.process.binary` PA property may be `bwrap` or similar. Always support matching on `application.flatpak.id` in addition to binary/name.
- **Immutable OS.** `/usr` is read-only. `/etc` is a writable overlay. User home is writable. udev rules go in `/etc/udev/rules.d/`. Binaries go in `~/.cargo/bin/` or similar user-writable location.

## Code Review Checklist

Before considering any change complete:

1. `cargo clippy --workspace -- -D warnings` and `cargo fmt --check --all` are clean
2. `just test` passes (or `distrobox enter pcpaneld-dev -- cargo test --workspace`) and `just deny` is clean
3. No `unwrap()` added outside of tests
4. Error messages include enough context to diagnose without a debugger
5. New public types derive `Debug` at minimum
6. New channels have documented bounds and overflow strategy
7. Tests cover the failure modes, not just the happy path
8. No unnecessary dependencies added
9. No premature abstractions — solve the concrete problem first
