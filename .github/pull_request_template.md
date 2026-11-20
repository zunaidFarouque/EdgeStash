## Summary

Provide a concise overview of the changes in this pull request and the motivation behind them.

- Closes #
- Relates to #

## Architectural & Performance Checklist

Please verify each of the following before requesting review:

- [ ] **Zero Idle CPU**: No busy loops (`while true`), polling threads, or periodic tick timers when idle.
- [ ] **DAW / Audio Safety**: No dynamic memory allocations in high-frequency mouse hook or DPC-sensitive paths.
- [ ] **Least Privilege (`asInvoker`)**: Verified that no administrator or elevated privileges are required.
- [ ] **Tests Pass**: Ran `cargo test --workspace -- --test-threads=1` and all workspace tests pass.
- [ ] **Zero Clippy Warnings**: Ran `cargo clippy --workspace --all-targets -- -D warnings` with zero warnings.
- [ ] **Formatted Code**: Ran `cargo fmt --check` cleanly.
- [ ] **Multi-Monitor & DPI**: Tested or designed with per-monitor DPI scaling awareness.

## Type of Change

- [ ] Bug fix (non-breaking fix for an issue)
- [ ] New feature (non-breaking addition to functionality)
- [ ] Performance optimization (improves latency, RAM, or CPU efficiency)
- [ ] Refactoring (internal cleanup without behavior changes)
- [ ] Documentation update

## Verification & Testing Performed

Describe the testing performed to validate these changes (e.g., unit test names, manual multi-monitor validation, gesture testing).
