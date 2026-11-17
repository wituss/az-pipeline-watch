# Copilot instructions

## Build, test, and lint

- Check formatting: `cargo fmt --check`
- Run Rust unit tests: `cargo test --locked --release`
- Run one Rust unit test: `cargo test --locked --release <test_name>`
- Build the release binary: `cargo build --locked --release`
- Run terminal integration tests: `python3 tests/terminal_integration.py target/release/az-pipeline-watch`
- Run one terminal integration test: `python3 tests/terminal_integration.py target/release/az-pipeline-watch TerminalIntegrationTests.test_selects_pending_stage_and_approves_only_uat`

The terminal integration tests use a local `az` double and do not contact Azure. On Windows, install `tests/requirements.txt` first; the tests use ConPTY there and POSIX PTY on Linux/macOS.

## Architecture

The CLI starts in `src/main.rs`: it parses `Args`, validates the build-results URL into a `Target`, and chooses either non-interactive monitoring or interactive stage selection. `Azure` in `src/azure.rs` implements the `Api` trait by invoking the user's installed `az` CLI (`az devops invoke` for build, timeline, and approvals; `az rest` for source-change metadata). `src/model.rs` deserializes those responses and maps nested checkpoint approvals to their owning stages by walking timeline parents.

Approval scope is kept separate from API and polling logic: `src/policy.rs` maps flags or an interactive selection to stage IDs, and `src/watch.rs` polls and applies that policy. `src/display.rs`, `src/interactive.rs`, and `src/controls.rs` handle output, stage selection, and terminal input/restoration. Unit tests in `src/tests.rs` inject a fake `Api`; terminal end-to-end coverage is in `tests/terminal_integration.py`, which runs the built binary against a local Azure CLI double.

## Repository-specific behavior

- Monitoring with no approval flags is read-only. `--once` does not suppress writes when approval flags are present; use `--dry-run` to preview without PATCH requests. Treat approval-scope changes as behavior-sensitive, especially `--approve-all`, which can include production.
- Stage selection and `--approve-through` use timeline order, not inferred dependencies. The through-stage boundary is inclusive and may include earlier parallel stages.
- Approval handling fails closed: verify the build is in progress and the approval is still actionable immediately before writing, then read it back and confirm the vote. Do not add automatic retries for failed or uncertain writes.
- Keep Azure calls behind `azure::Api` so behavior can be tested with fakes without credentials or network access. The PTY integration suite covers terminal behavior and uses a local `az` fixture rather than a real tenant.
- Interactive terminal setup is managed by `TerminalInput`; preserve its cleanup behavior so raw mode and cursor visibility are restored on exit and errors.
- Releases are driven by `.github/workflows/release.yml` on pushes to `master`. It increments the stable patch version in `Cargo.toml` and `Cargo.lock`, runs formatting, release tests/builds and terminal integration tests across Linux, Windows, and macOS, then tags and publishes only after all platform jobs succeed.
