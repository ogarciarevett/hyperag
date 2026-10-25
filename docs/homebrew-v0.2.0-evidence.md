# Homebrew v0.2.0 verification

On 2026-10-25, the Hyperag v0.2.0 formula passed strict online audit, installation, and its version test on macOS arm64 with Homebrew 7.0.10.

The public [tap](https://github.com/ogarciarevett/homebrew-tap) is pinned to commit `b205130cacc3b475584515a7486297ae98e4d94c`. Its formula uses the four platform hashes from [v0.2.0 SHA256SUMS](https://github.com/ogarciarevett/hyperag/releases/download/v0.2.0/SHA256SUMS). The Apache-2.0 license matches Hyperag.

## Audit and generator

The original generator from release commit `3f3f1a91b7ca081e3662117a633e36483bb4df63` emitted an explicit version that strict audit rejected:

```text
Error: 1 problem in 1 formula detected.
ogarciarevett/tap/hyperag
  * Stable: `version 0.2.0` is redundant with version scanned from URL
```

Removing that declaration lets Homebrew infer the version from the release URL. The generator fix produces byte-for-byte the published formula from the same release checksums. Its regression test failed before the fix and all 3 generator tests passed afterward. Existing CI already runs `python3 -I scripts/test_homebrew_formula.py`.

```sh
brew audit --strict --online ogarciarevett/tap/hyperag
```

Exit status: 0, with no output. The audit and installation used a local staging tap with the same formula as the published commit and the permanent GitHub origin.

## Installation and version

```text
$ brew install ogarciarevett/tap/hyperag
==> Trusted formula ogarciarevett/tap/hyperag
==> Fetching downloads for: hyperag
✔︎ Formula hyperag (0.2.0)
==> Installing hyperag from ogarciarevett/tap
🍺  /opt/homebrew/Cellar/hyperag/0.2.0: 7 files, 36MB, built in 1 second
$ hyperag --version
hyperag 0.2.0
```

Both commands exited 0. `brew test ogarciarevett/tap/hyperag` also exited 0 and ran the formula's installed-binary version assertion.

## Publication and cleanup

The new repository received one initial commit directly on `main`, followed by active ruleset `protect-default-branch` (ID `25817702`): pull requests required, force pushes and deletion blocked, repository administrator bypass retained. The tap has no CI, so no required check was added.

```text
$ brew uninstall hyperag
Uninstalling /opt/homebrew/Cellar/hyperag/0.2.0... (7 files, 36MB)
$ brew untap ogarciarevett/tap
Untapping ogarciarevett/tap...
Untapped 1 formula (18 files, 22.5KB).
```

Both cleanup commands exited 0. The original five taps remain, Hyperag is absent from the installed formula list and PATH, and developer mode was restored to off. Homebrew's downloaded audit gems and caches remain. The original `hyperag-public` checkout remains clean at the v0.2.0 commit.

This verifies installation and version execution on macOS arm64. Other platform binaries and service/database behavior were not exercised.
