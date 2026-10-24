# Install Hyperag

The Rust binary provides the service, CLI and MCP server. It needs Postgres with pgvector to store memory; the default graph channel also needs Neo4j. Installing the binary does not start either database.

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/ogarciarevett/hyperag/main/scripts/install.sh | sh
```

Verify with `~/.local/bin/hyperag --version`. Add `~/.local/bin` to your PATH if needed. `HYPERAG_VERSION` pins a release tag; `HYPERAG_INSTALL_DIR` selects the destination. The installer verifies the archive using the release's SHA256SUMS file before copying the binary.

## Windows x64

```powershell
irm https://raw.githubusercontent.com/ogarciarevett/hyperag/main/scripts/install.ps1 | iex
```

Verify with `& "$env:LOCALAPPDATA\hyperag\bin\hyperag.exe" --version`. `HYPERAG_VERSION` and `HYPERAG_INSTALL_DIR` work here too.

## Homebrew and registries

The Homebrew formula, Rust crate and npm package are being prepared for the first public release. Do not assume registry names or release assets are available until the release checks finish. The binary installers above require published release assets.

## Build from source

```sh
git clone https://github.com/ogarciarevett/hyperag && cd hyperag
cargo build --release -p hyperag
./target/release/hyperag --version
```

Use the Rust version pinned in the release workflow. The TypeScript reference and benchmark harness use the Bun version pinned in CI; `bun install --frozen-lockfile` installs their dependencies.

Linux release binaries are built on Ubuntu 22.04 (glibc 2.35). Their ONNX CPU runtime is built from pinned upstream source on the same host; Intel macOS also uses that source build because the dependency does not provide an Intel prebuilt. ONNX build revision and license notices are included in those archives.
