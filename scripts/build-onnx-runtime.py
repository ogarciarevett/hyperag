#!/usr/bin/env python3
"""Build the API-24 CPU runtime on the release host instead of a newer-ABI prebuilt."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys


REVISION = "3a728b75062256951b6e19ce718907cf1a1d4cf0"  # microsoft/onnxruntime v1.24.3


def run(*args, **kwargs):
    subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    library = out / "lib" / "libonnxruntime.a"
    manifest = out / "lib" / "build.json"
    identity = {"format": 2, "revision": REVISION, "system": platform.system(), "machine": platform.machine()}
    if manifest.exists() and library.exists():
        cached = json.loads(manifest.read_text())
        if all(cached.get(key) == value for key, value in identity.items()):
            if cached.get("sha256") == hashlib.sha256(library.read_bytes()).hexdigest():
                print("Verified cached source-built ONNX CPU runtime", flush=True)
                return
    source = out / "source"
    build = out / "build"
    out.mkdir(parents=True, exist_ok=True)
    # Never reuse an incomplete source/build tree; only the verified archive is cached.
    for directory in (source, build):
        if directory.exists():
            shutil.rmtree(directory)
    run("git", "init", source)
    run("git", "-C", source, "remote", "add", "origin", "https://github.com/microsoft/onnxruntime.git")
    run("git", "-C", source, "fetch", "--depth=1", "origin", REVISION)
    run("git", "-C", source, "checkout", "--detach", "FETCH_HEAD")
    command = [sys.executable, source / "tools/ci_build/build.py", "--build_dir", build,
               "--config", "Release", "--update", "--build", "--parallel", "2", "--skip_tests",
               "--cmake_extra_defines", "onnxruntime_BUILD_UNIT_TESTS=OFF", "BUILD_SHARED_LIBS=OFF"]
    run(*command)
    # Upstream excludes RE2 from ALL; static-only builds do not otherwise build it.
    run("cmake", "--build", build / "Release", "--config", "Release", "--target", "re2", "--parallel", "2")
    archives = sorted((build / "Release").rglob("*.a"))
    if not any(path.name == "libre2.a" for path in archives):
        raise RuntimeError("ONNX static runtime is incomplete: libre2.a is missing")
    library.parent.mkdir(parents=True, exist_ok=True)
    if library.exists():
        library.unlink()
    if platform.system() == "Darwin":
        run("/usr/bin/libtool", "-static", "-o", library, *archives)
    elif platform.system() == "Linux":
        # MRI merges archive members; passing .a files to ordinary `ar r` would nest them.
        if any(any(char.isspace() for char in str(path)) for path in [library, *archives]):
            raise RuntimeError("Linux archive paths must not contain whitespace")
        instructions = [f"CREATE {library}", *[f"ADDLIB {path}" for path in archives], "SAVE", "END"]
        run("ar", "-M", input="\n".join(instructions) + "\n", text=True)
        run("ranlib", library)
    else:
        raise RuntimeError("This source builder supports Linux and macOS")
    run("ar", "t", library, stdout=subprocess.DEVNULL)
    for name in ("LICENSE", "ThirdPartyNotices.txt"):
        shutil.copyfile(source / name, library.parent / name)
    identity["sha256"] = hashlib.sha256(library.read_bytes()).hexdigest()
    manifest.write_text(json.dumps(identity, indent=2) + "\n")
    print(json.dumps(identity), flush=True)


if __name__ == "__main__":
    main()
