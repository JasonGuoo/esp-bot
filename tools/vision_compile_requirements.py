#!/usr/bin/env python3
"""Register the pinned MSR/MNP int8 vision family with ESP-DL's own stripper.

ESP-SR already registers its exact audio subset. The older face component has
no sidecar: retain its operators and every per-tensor int8 kernel, rather
than disabling stripping globally (which retains megabytes of unused kernels).
"""
import argparse
import importlib.util
import hashlib
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--espdl", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--models", type=Path, required=True)
    args = parser.parse_args()
    # These official 0.5.2 models contain Conv/Concat/PRelu, S8 activations and
    # weights, and exactly one exponent per tensor (not per-channel). Fail
    # closed on a model update instead of silently stripping its new kernels.
    expected = {
        "human_face_detect_msr_s8_v1.espdl": "2bbaaf17f39ce9ee547abf5ffdc6d8fcd1844009a0d26878d7bfb33451f16211",
        "human_face_detect_mnp_s8_v1.espdl": "5b7fedcff59dbe69d1935d6d206bf371fc18b5dd9176320ef69c9f05ee83af55",
    }
    for name, digest in expected.items():
        if hashlib.sha256((args.models / name).read_bytes()).hexdigest() != digest:
            raise SystemExit(f"Vision model changed; regenerate and validate its compile requirements: {name}")
    spec = importlib.util.spec_from_file_location("espdl_compile", args.espdl / "cmake/compile_finalize.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    ops = module.load_yaml(args.espdl / "spec/ops.yml")["ops"]
    kernels = module.load_yaml(args.espdl / "spec/kernels.yml")
    int8 = sorted(name for name in kernels["symbols"] if "_s8_" in name and
                  "per_channel" not in name and "per_ch_" not in name)
    if not int8 or "Concat" not in ops or "Conv" not in ops:
        raise SystemExit("Unsupported ESP-DL op/kernel registry")
    text = "# Generated for the pinned MSR/MNP s8 vision models; audio requirements remain registered.\n"
    text += f"kernel_abi: {kernels['kernel_abi']}\nops:\n"
    text += "".join(f"  - {name}\n" for name in ["Concat", "Conv", "PRelu"])
    text += "kernels:\n" + "".join(f"  - {name}\n" for name in int8)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if not args.output.exists() or args.output.read_text() != text:
        args.output.write_text(text)


if __name__ == "__main__":
    main()
