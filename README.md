# az-pipeline-watch

A Rust CLI for monitoring Azure DevOps YAML pipeline runs and submitting
environment/check approvals using your existing Azure CLI credentials.

The watcher does not start builds, approve `ManualValidation` tasks or classic
releases, or bypass checks and permissions.

## Requirements

- Linux, Windows, or macOS. Interactive controls use `crossterm`; no `stty`
  executable is required.
- Azure CLI with the Azure DevOps extension and an authenticated account with
  access to the build. Approvals require the appropriate Azure DevOps permissions.
- Stable Rust and a C linker if building from source.

```bash
az extension add --name azure-devops
az login
```

## Install

Download a Linux x86_64, Windows x86_64, or macOS Apple Silicon (arm64) archive
from [Releases](https://github.com/wituss/az-pipeline-watch/releases),
extract it, and place the executable on your `PATH`. Each archive includes a
separate SHA-256 checksum file.

Alternatively, build and install from source:

```bash
git clone https://github.com/wituss/az-pipeline-watch.git
cd az-pipeline-watch
cargo install --path . --locked
```

## Usage

Always quote build URLs, especially when they contain `&`.

### Monitor a run

```bash
az-pipeline-watch 'https://dev.azure.com/ORG/PROJECT/_build/results?buildId=123'
```

Supplying a URL without approval flags is read-only. Status refreshes every
30 seconds in place in an interactive terminal; redirected output is plain text.

While waiting between polling cycles:

| Key | Action |
| --- | --- |
| `r` | Refresh immediately |
| `+` or `=` | Decrease the interval by 5 seconds, down to 1 second |
| `-` | Increase the interval by 5 seconds, up to 3,600 seconds |
| `Ctrl-C` | Stop monitoring |

```bash
# Inspect stages and existing approval status without submitting votes.
az-pipeline-watch 'BUILD_URL' --list-stages

# Read status once.
az-pipeline-watch 'BUILD_URL' --once

# Poll every 10 seconds.
az-pipeline-watch 'BUILD_URL' --interval 10
```

Replace `BUILD_URL` with the full Azure DevOps build results URL.

### Interactive stage selection

Start without a URL:

```bash
az-pipeline-watch
```

Enter the build URL, then use Up/Down to navigate, Space to select stages,
and Enter to continue. Type `approve` to confirm; `q` or Escape cancels selection.
Stages may be selected before their approvals become available. The build must
be in progress.

After confirmation, this mode monitors the run and submits approvals as they
become available for the selected stages. Use `--once` to check only once or
`--dry-run` to preview without submitting approvals.

### Approve through a stage

Preview first:

```bash
az-pipeline-watch 'BUILD_URL' --approve-through Stage_uat --dry-run --once
```

Then submit approvals and continue monitoring:

```bash
az-pipeline-watch 'BUILD_URL' --approve-through Stage_uat
```

The stage ID must match exactly. All stages in timeline order through that stage
are included, not just its dependencies. Parallel stages ordered before the
target may therefore also be approved. Later stages are not approved, but the
watcher continues monitoring the run.

**Approval flags authorize writes without an additional confirmation prompt.**
`--once` still submits approvals unless paired with `--dry-run`.

To preview every available approval, potentially including production:

```bash
az-pipeline-watch 'BUILD_URL' --approve-all --dry-run --once
```

Remove `--dry-run` only if you intend to approve that entire scope.

Use `--request-timeout SECONDS` to change the timeout for individual Azure CLI
calls (default: 120 seconds). Run `az-pipeline-watch --help` for all options.

## Development and releases

```bash
cargo fmt --check
cargo test --locked --release
cargo build --locked --release
python3 tests/terminal_integration.py target/release/az-pipeline-watch
```

Terminal integration tests run on all three platforms using POSIX PTY on Linux
and macOS, or ConPTY through `pywinpty` on Windows, with a local Azure CLI double.
On Windows, install the test dependencies with
`python -m pip install -r tests/requirements.txt` before running the tests.
They do not contact Azure or submit real approvals.
They cover selection, delayed approval availability, confirmation, dry-run,
cancellation, API errors, keyboard controls, and terminal restoration.

Pushes to `master` trigger GitHub Actions. The workflow increments the highest
stable version found in `Cargo.toml` or Git tags by one patch, updates the
manifest and lockfile, checks formatting, runs tests, and builds the binary.
Builds and tests run on Linux x86_64, Windows x86_64, and macOS arm64.
Only after all platforms succeed does it commit the version bump, push a
`vX.Y.Z` tag, and publish a GitHub Release with archives and checksums for
all three platforms. Windows binaries are packaged as ZIP files; Linux and
macOS binaries use tar.gz archives.
