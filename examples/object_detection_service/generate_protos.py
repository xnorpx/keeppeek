# SPDX-License-Identifier: AGPL-3.0-only
"""Generate typed Python bindings from KeepPeek's canonical protobuf schema."""

import shutil
import subprocess
import sys
from pathlib import Path

MIT_SPDX_HEADER = "# SPDX-License-Identifier: MIT\n"


def main() -> int:
    example_dir = Path(__file__).resolve().parent
    repository_root = example_dir.parents[1]
    proto_dir = repository_root / "api"
    output_dir = example_dir / "generated"
    plugin_name = "protoc-gen-mypy.exe" if sys.platform == "win32" else "protoc-gen-mypy"
    environment_plugin = Path(sys.executable).with_name(plugin_name)
    path_plugin = shutil.which("protoc-gen-mypy")
    plugin = environment_plugin if environment_plugin.is_file() else path_plugin
    if plugin is None:
        print("protoc-gen-mypy is unavailable; install requirements.txt", file=sys.stderr)
        return 1
    output_dir.mkdir(parents=True, exist_ok=True)
    command = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"--proto_path={proto_dir}",
        f"--python_out={output_dir}",
        f"--mypy_out={output_dir}",
        f"--plugin=protoc-gen-mypy={plugin}",
        str(proto_dir / "webrtc.proto"),
    ]
    completed = subprocess.run(command, check=False)
    if completed.returncode != 0:
        print("KeepPeek protobuf generation failed", file=sys.stderr)
        return completed.returncode
    for generated_name in ("webrtc_pb2.py", "webrtc_pb2.pyi"):
        generated_path = output_dir / generated_name
        generated = generated_path.read_text(encoding="utf-8")
        if not generated.startswith(MIT_SPDX_HEADER):
            generated_path.write_text(MIT_SPDX_HEADER + generated, encoding="utf-8")
    print(f"Generated typed bindings in {output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
