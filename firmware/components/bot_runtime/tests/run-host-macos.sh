#!/bin/sh
# Exercise bot_runtime_step with the existing host bus and the board's settings.
set -eu
firmware_dir=$(cd "$(dirname "$0")/../../.." && pwd)
components="$firmware_dir/components"
case "${1:-runtime}" in
    runtime) test_source=test_runtime_near_face.c ;;
    reminders) test_source=test_proactive.c ;;
    simulator) test_source=simulator.c ;;
    *) echo 'Expected runtime, reminders or simulator' >&2; exit 2 ;;
esac
idf="$firmware_dir/.toolchains/esp-idf/components"
config="$firmware_dir/build/config"
if [ ! -f "$config/sdkconfig.h" ]; then
    echo 'Generate the board configuration locally with firmware/idf.sh reconfigure first.' >&2
    exit 1
fi

scratch=$(mktemp -d "${TMPDIR:-/tmp}/espbot-runtime.XXXXXXXX")
trap 'rm -rf "$scratch"' EXIT
cat > "$scratch/sdkconfig.h" <<'HEADER'
#include_next "sdkconfig.h"
#undef CONFIG_FREERTOS_NUMBER_OF_CORES
#define CONFIG_FREERTOS_NUMBER_OF_CORES 1
#undef CONFIG_FREERTOS_UNICORE
#define CONFIG_FREERTOS_UNICORE 1
#define CONFIG_IDF_TARGET_LINUX 1
HEADER
cat > "$scratch/replies.S" <<ASM
.section __TEXT,__const
.globl _binary_wake_replies_adpcm_start, _binary_wake_replies_adpcm_end
_binary_wake_replies_adpcm_start:
.incbin "$components/bot_responses/wake_replies.adpcm"
_binary_wake_replies_adpcm_end:
ASM

set -- "-I$scratch" "-I$config"
for subdir in esp_common/include esp_system/include esp_hw_support/include/linux esp_hw_support/include \
    soc/linux/include soc/include freertos/config/include freertos/config/include/freertos \
    freertos/config/linux/include freertos/FreeRTOS-Kernel/include \
    freertos/FreeRTOS-Kernel/portable/linux/include freertos/FreeRTOS-Kernel/portable/linux/include/freertos; do
    set -- "$@" "-I$idf/$subdir"
done
for name in bot_runtime bot_bus bot_voice bot_responses attention_manager vision_attention; do
    set -- "$@" "-I$components/$name/include"
done
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"
xcrun clang -isysroot "$(xcrun --sdk macosx --show-sdk-path)" -std=gnu11 -g -O1 \
    -fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer "$@" \
    "$components/bot_runtime/tests/$test_source" "$components/bot_runtime/bot_runtime.c" \
    "$components/attention_manager/attention_manager.c" "$components/bot_voice/bot_voice.c" \
    "$components/bot_responses/bot_responses.c" "$scratch/replies.S" -o "$scratch/runtime"
"$scratch/runtime"
if [ "$test_source" != simulator.c ]; then echo "$test_source passed (ASan and UBSan)."; fi
