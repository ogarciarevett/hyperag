# Homebrew tap formula

Generate `Formula/hyperag.rb` in `ogarciarevett/homebrew-tap` from the first public release's actual SHA256SUMS file. The generator refuses missing/duplicate/invalid checksums and pins all four macOS/Linux architectures. No placeholder formula is published.

After cloning the tap, the owner can run this from the Hyperag checkout (substitute the verified tag):

```sh
gh release download v0.2.0 --repo ogarciarevett/hyperag --pattern SHA256SUMS --dir /tmp/hyperag-release-checksums --clobber && python3 scripts/homebrew-formula.py --version v0.2.0 --checksums /tmp/hyperag-release-checksums/SHA256SUMS --out ../homebrew-tap/Formula/hyperag.rb
```

Homebrew audits and installs a formula by its tapped name. For a new tap, commit the generated formula locally first, then register that clone with `brew tap ogarciarevett/tap "$(cd ../homebrew-tap && pwd)"`. The local commit makes the formula visible in Homebrew's clone; push it only after validation.

If the tap is already installed, update the formula in the checkout reported by `brew --repo ogarciarevett/tap` instead of tapping a different clone URL. Preserve local changes before updating it.

Run `brew audit --strict --online ogarciarevett/tap/hyperag` and `brew install --build-from-source ogarciarevett/tap/hyperag` against the newly generated formula, then commit any remaining change. Before pushing from that tap checkout, set its origin to `https://github.com/ogarciarevett/homebrew-tap.git` so publication and future updates use GitHub. The expected user command after publication is `brew install ogarciarevett/tap/hyperag`.
