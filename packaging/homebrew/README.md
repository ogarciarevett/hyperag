# Homebrew tap formula

Generate `Formula/hyperag.rb` in `ogarciarevett/homebrew-tap` from the first public release's actual SHA256SUMS file. The generator refuses missing/duplicate/invalid checksums and pins all four macOS/Linux architectures. No placeholder formula is published.

After cloning the tap, the owner can run this from the Hyperag checkout (substitute the verified tag):

```sh
gh release download v0.2.0 --repo ogarciarevett/hyperag --pattern SHA256SUMS --dir /tmp/hyperag-release-checksums --clobber && python3 scripts/homebrew-formula.py --version v0.2.0 --checksums /tmp/hyperag-release-checksums/SHA256SUMS --out ../homebrew-tap/Formula/hyperag.rb
```

Review the formula, run `brew audit --strict --online ../homebrew-tap/Formula/hyperag.rb` and `brew install --build-from-source ../homebrew-tap/Formula/hyperag.rb`, then commit/push in the tap. The expected user command after the tap is published is `brew install ogarciarevett/tap/hyperag`.
