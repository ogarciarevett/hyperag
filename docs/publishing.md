# Publish packages after the first public release

Repository publication, release assets and registry publication are separate actions. Registry accounts and credentials belong to the owner; these instructions create neither accounts nor tokens. Verify package contents and the release tag before publishing.

## Owner commands

Rust crate (after owner login):

```sh
cargo login && cargo publish --locked --manifest-path crates/hyperag/Cargo.toml
```

npm reference library (after owner login; the prepack step builds JavaScript and declarations):

```sh
npm login && npm publish --access public
```

These commands use each registry's own interactive login. Do not put tokens in arguments, fixtures or documentation. Check name availability/ownership before publishing; a successful dry run is not registry acceptance.

## Plugin marketplace

The root marketplace manifest supports installing the existing MCP/hooks plugin from this repository. It requires a running Hyperag service and a token restricted to the user's scopes. It does not provision a database or register a public service endpoint.

```sh
claude plugin marketplace add ogarciarevett/hyperag && claude plugin install hyperag@hyperag-local
```

## Homebrew

After the release assets and SHA256SUMS exist, generate the versioned formula with `scripts/homebrew-formula.py`. Review and publish that file in `ogarciarevett/homebrew-tap/Formula/hyperag.rb`. The formula selects the correct macOS/Linux architecture and uses the release checksums; do not publish placeholder hashes.

## Verification before publishing

- `cargo package --list --manifest-path crates/hyperag/Cargo.toml` and `cargo publish --dry-run --locked --manifest-path crates/hyperag/Cargo.toml` inspect/build the crate without uploading.
- `npm pack` builds the JavaScript/declarations and creates a local archive without uploading; run `node scripts/package-smoke.mjs . hyperag-0.2.0.tgz` to check its SQL assets, runtime migration loading and a NodeNext consumer before `npm publish`.
- Confirm README, LICENSE, NOTICE and built library declarations are included, with no operational files, credentials, private paths or model caches.
- The release workflow separately builds platform binaries and runs a clean-container installer check. Its artifacts and logs are the evidence for installer acceptance.
