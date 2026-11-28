"""Compile the actual portable C implementations with Mac ASan/UBSan.

The runtime uses its existing runner and real project IDF headers. ESP-SR's
decoder is the fixture in test_keyword_boundaries.c, not an acoustic claim.
"""
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
FW = ROOT / "firmware"
C = FW / "components"
SOURCES = {
    "attention/test_attention": ["attention/attention.c"],
    "attention/test_idle_attention": ["attention/idle_attention.c"],
    "vision_attention/test_vision_attention": ["vision_attention/vision_attention.c", "vision_attention/pupil_candidate.c"],
    "face/test_eve_pose": [], "face/test_eve_gaze": [], "face/test_gaze_smoothing": [],
    "bot_audio/test_followup_vad": [],
    "bot_audio/test_keyword_boundaries": ["bot_audio/bot_audio_keyword.c", "bot_voice/bot_voice.c"],
    "bot_vision/test_packed_pixels": [], "bot_platform/test_rgb565_frame": [],
    "bot_runtime/test_ambient_light": [], "bot_runtime/test_idle_expression": [],
    "bot_runtime/test_near_face": [], "bot_runtime/test_passive_cycle": [],
    "bot_responses/test_selection": ["bot_responses/bot_responses.c"],
}


def call(argv):
    subprocess.run([str(arg) for arg in argv], cwd=ROOT, check=True)


def main():
    os.environ.setdefault("DEVELOPER_DIR", "/Library/Developer/CommandLineTools")
    discovered = {f"{p.parents[1].name}/{p.stem}" for p in C.glob("*/tests/test_*.c")}
    registered = set(SOURCES) | {"bot_runtime/test_runtime_near_face", "bot_runtime/test_proactive"}
    if discovered != registered:
        raise SystemExit(f"Register new/removed C cases: {discovered ^ registered}")
    sdk = subprocess.check_output(["xcrun", "--sdk", "macosx", "--show-sdk-path"], text=True).strip()
    with tempfile.TemporaryDirectory(prefix="espbot-regression-c-") as tmp:
        scratch = Path(tmp)
        (scratch / "sdkconfig.h").write_text('#include_next "sdkconfig.h"\n#undef CONFIG_FREERTOS_NUMBER_OF_CORES\n#define CONFIG_FREERTOS_NUMBER_OF_CORES 1\n#undef CONFIG_FREERTOS_UNICORE\n#define CONFIG_FREERTOS_UNICORE 1\n#define CONFIG_IDF_TARGET_LINUX 1\n')
        assembly = scratch / "replies.S"
        assembly.write_text(f'.section __TEXT,__const\n.globl _binary_wake_replies_adpcm_start, _binary_wake_replies_adpcm_end\n_binary_wake_replies_adpcm_start:\n.incbin "{C / "bot_responses/wake_replies.adpcm"}"\n_binary_wake_replies_adpcm_end:\n')
        includes = [scratch, FW / "build/config", FW / "main"]
        for folder in C.iterdir():
            if folder.is_dir():
                includes += [folder, folder / "include"]
        idf = FW / ".toolchains/esp-idf/components"
        includes += [idf / sub for sub in (
            "esp_common/include", "esp_system/include", "esp_hw_support/include/linux", "esp_hw_support/include",
            "soc/linux/include", "soc/include", "soc/esp32s31/include", "freertos/config/include", "freertos/config/include/freertos",
            "freertos/config/linux/include", "freertos/FreeRTOS-Kernel/include", "freertos/FreeRTOS-Kernel/portable/linux/include",
            "freertos/FreeRTOS-Kernel/portable/linux/include/freertos", "heap/include", "log/include", "esp_timer/include", "esp_rom/include",
            "esp_hw_support/etm/include", "hal/include")]
        includes += [FW / "managed_components/espressif__esp-sr/include/esp32s31", FW / "managed_components/espressif__esp-sr/src/include"]
        flags = ["xcrun", "clang", "-isysroot", sdk, "-std=gnu11", "-g", "-O1", "-fsanitize=address,undefined",
                 "-fno-sanitize-recover=all", "-fno-omit-frame-pointer", "-ffunction-sections", "-Wl,-dead_strip"]
        flags += [f"-I{p}" for p in includes]
        for name, dependencies in SOURCES.items():
            component, test = name.split("/")
            sources = [C / component / "tests" / (test + ".c"), *[C / dep for dep in dependencies]]
            if component == "bot_responses":
                sources.append(assembly)
            binary = scratch / test
            call([*flags, *sources, "-o", binary])
            call([binary])
            print(f"REGRESSION_CASE {name} passed", flush=True)
    for option, name in (("runtime", "test_runtime_near_face"), ("reminders", "test_proactive")):
        call([C / "bot_runtime/tests/run-host-macos.sh", option])
        print(f"REGRESSION_CASE bot_runtime/{name} passed", flush=True)


if __name__ == "__main__":
    main()
