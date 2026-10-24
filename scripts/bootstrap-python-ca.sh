#!/bin/sh
# Upgrade the package downloader by verified hash before other package downloads.
set -eu
python_command=${1:-python3}
"$python_command" -c 'import sys; sys.exit("Python >=3.10 is required for patched pip") if sys.version_info < (3, 10) else None'
"$python_command" -m pip install "pip @ https://files.pythonhosted.org/packages/3f/06/3e4420302363fbb71be3c7f35ce042f54bfca330e3e66b3a2e3c4cca579f/pip-26.2.2-py3-none-any.whl#sha256=fd7f4dfa8190e014013b9f08f3e973f4e4c26ffd179a204895533a7254813a70"
"$python_command" -m pip install "certifi>=2026.10.19" "uv>=0.13.4"
"$python_command" -c 'from importlib.metadata import version; assert tuple(map(int, version("pip").split("."))) >= (26, 2, 2); assert tuple(map(int, version("certifi").split("."))) >= (2026, 10, 19); assert tuple(map(int, version("uv").split("."))) >= (0, 13, 4)'
