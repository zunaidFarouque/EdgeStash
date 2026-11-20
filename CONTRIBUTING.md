# Contributing to EdgeStash

Thank you for your interest in contributing to EdgeStash! We welcome contributions ranging from bug reports and documentation fixes to new edge gestures and performance optimizations.

Because EdgeStash is built for professional desktop workflows—including low-latency real-time audio (DAWs) and VJ setups—we adhere to rigorous engineering and performance standards.

---

## Architectural Invariants & Guarantees

Before writing code or opening a PR, please review these non-negotiable project invariants:

1. **Zero Idle CPU**:
   - The background daemon (`edgestash-daemon`) must never spin in busy loops (`while true`) or use continuous polling timers.
   - All state transitions are purely event-driven, responding to Win32 window hooks, DWM messages, and named pipe IPC requests.
2. **Audio & DAW Latency Safety**:
   - The low-level mouse hook (`WH_MOUSE_LL`) must execute in sub-microsecond time.
   - Never perform heap allocations (`Box`, `Vec`, `String`), file I/O, or blocking synchronization inside the mouse hook callback.
   - Do not call aggressive memory trimming APIs (e.g. `SetProcessWorkingSetSize(-1, -1)`) that cause page faults and DPC latency spikes.
3. **Least Privilege Execution (`asInvoker`)**:
   - EdgeStash must run cleanly with standard user rights.
   - Manifests must specify `<requestedExecutionLevel level="asInvoker" uiAccess="false"/>`.
4. **Zero Clippy Warnings & Strict TDD**:
   - All code must pass `cargo clippy --workspace --all-targets -- -D warnings`.
   - Any bug fix or new feature must be accompanied by unit or integration tests in `crates/*/tests/`.

---

## Workspace Structure

The workspace is organized into four modular Rust crates:

- **`crates/edgestash-ipc`**: Core domain types, geometric primitives, slot data structures, and cross-process IPC protocols over Windows named pipes.
- **`crates/edgestash-daemon`**: Background engine handling low-level Win32 hooks (`WH_MOUSE_LL`), gesture detection (Corner Churn, Edge Bump, Edge Hover), window movement, and Z-order management.
- **`crates/edgestash-cli`**: Command-line interface (`edgestash.exe`) for querying status, triggering slots, and controlling daemon state.
- **`crates/edgestash-ui`**: Modern Fluent-styled configuration GUI and tray host built with DirectComposition and Direct2D.

---

## Development Setup

### Prerequisites

- **Operating System**: Windows 10 (Build 19041+) or Windows 11 (recommended).
- **Rust Toolchain**: Stable Rust 1.85+ with MSVC target (`x86_64-pc-windows-msvc`).
- **Visual Studio Build Tools**: C++ build tools with Windows 10/11 SDK.

### Building & Running

Clone the repository and compile the workspace:

```pwsh
# Build debug binaries
cargo build

# Build optimized release binaries
cargo build --release
```

### Running Tests

Because several integration tests instantiate Win32 window message loops, run tests sequentially:

```pwsh
cargo test --workspace -- --test-threads=1
```

### Checking Linters and Code Formatting

Ensure your code matches project standards:

```pwsh
# Verify code formatting
cargo fmt --check

# Check with clippy (warnings are treated as errors)
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Pull Request Guidelines

1. **Create a Topic Branch**: Branch off `master` with a descriptive name (e.g., `fix/drag-image-preview` or `feat/corner-threshold-tuning`).
2. **Follow TDD**: Write a failing test first demonstrating the issue or feature requirement, then implement the solution.
3. **Run Full Verification**:
   ```pwsh
   cargo fmt --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace -- --test-threads=1
   ```
4. **Submit Pull Request**: Open a PR using our pull request template, detailing your changes, verification results, and any relevant issue numbers.
