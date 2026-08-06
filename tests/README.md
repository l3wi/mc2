# Integration tests (`mcc-tests`)

Shared harness and cross-crate integration tests for MicroCommandControl.

**Policy:** use clean, directed **unit** tests (in `crates/*/src`) and **integration** tests (here + `crates/mcc/tests`) to keep behavior consistent and stop regressions. Full guide: [docs/guides/testing.md](../docs/guides/testing.md).

```bash
cargo test -p mcc-tests
# or
just test-integration
```
