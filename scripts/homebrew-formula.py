"""Generate the Homebrew formula from the published release checksums, never placeholder hashes."""
import argparse
from pathlib import Path
import re

TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu")


def formula(version, checksums):
    if not re.fullmatch(r"v?\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", version):
        raise ValueError("expected a semantic release version")
    hashes = {}
    for line in checksums.splitlines():
        parts = line.split()
        if len(parts) != 2 or not re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]):
            raise ValueError("invalid SHA256SUMS record")
        name = parts[1].lstrip("*")
        if name in hashes:
            raise ValueError("duplicate release asset checksum")
        hashes[name] = parts[0].lower()
    out = ['class Hyperag < Formula', '  desc "Shared memory and retrieval with scoped access and version history"',
           '  homepage "https://github.com/ogarciarevett/hyperag"', '  license "Apache-2.0"']
    for os_name, targets in [("macos", TARGETS[:2]), ("linux", TARGETS[2:])]:
        out += [f'  on_{os_name} do', '    if Hardware::CPU.arm?']
        for i, target in enumerate(targets):
            if i:
                out.append('    else')
            name = f"hyperag-{version}-{target}.tar.gz"
            if name not in hashes:
                raise ValueError("release is missing a platform checksum")
            out += [f'      url "https://github.com/ogarciarevett/hyperag/releases/download/{version}/{name}"', f'      sha256 "{hashes[name]}"']
        out += ['    end', '  end']
    out += ['  def install', '    bin.install "hyperag"', '    pkgshare.install "README.md", "LICENSE", "NOTICE"', '  end',
            '  test do', '    assert_match "hyperag #{version}", shell_output("#{bin}/hyperag --version")', '  end', 'end']
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--checksums", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    text = formula(args.version, args.checksums.read_text())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text)
    print("Generated versioned Homebrew formula from release checksums.")
