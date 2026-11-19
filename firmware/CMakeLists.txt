cmake_minimum_required(VERSION 3.16)
# ESP-IDF stores this release in the flashed app descriptor.
set(espbot_version_file "${CMAKE_CURRENT_LIST_DIR}/../VERSION")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${espbot_version_file}")
file(STRINGS "${espbot_version_file}" PROJECT_VER LIMIT_COUNT 1)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(esp_bot)

# Use the pinned BSP unchanged except for its display setup, which follows the
# upstream S31-Korvo factory example. Keep this override explicit and fail if
# an upstream version changes its source layout.
idf_component_get_property(espbot_bsp_lib espressif__esp32_s31_korvo_1 COMPONENT_LIB)
get_target_property(espbot_bsp_sources "${espbot_bsp_lib}" SOURCES)
set(espbot_display_source_count 0)
foreach(espbot_source IN LISTS espbot_bsp_sources)
  if(espbot_source MATCHES "(^|/)src/bsp_display\\.c$")
    list(REMOVE_ITEM espbot_bsp_sources "${espbot_source}")
    math(EXPR espbot_display_source_count "${espbot_display_source_count} + 1")
  endif()
endforeach()
if(NOT espbot_display_source_count EQUAL 1)
  message(FATAL_ERROR "Expected exactly one S31 BSP display source")
endif()
set_property(TARGET "${espbot_bsp_lib}" PROPERTY SOURCES "${espbot_bsp_sources}")
target_sources("${espbot_bsp_lib}" PRIVATE
  "${CMAKE_CURRENT_LIST_DIR}/components/bot_platform/bsp_display_factory.c")

# ESP-SR 2.6.1 registers only its neural-audio operator/kernel subset globally.
# human_face_detect 0.5.2 predates that stripping API and registers no model
# requirements, so the subset removes vision ops such as Concat and int8 Conv.
# Keep the pinned vision ops plus all per-tensor int8 kernels, while
# retaining ESP-SR's own audio requirements. Do not clear the audio sidecar:
# a full registry overflows the 8 MiB app once offline replies are embedded.
if(NOT CONFIG_FLASH_HUMAN_FACE_DETECT_MSRMNP_S8_V1 OR
   CONFIG_FLASH_ESPDET_PICO_224_224_FACE OR CONFIG_FLASH_ESPDET_PICO_416_416_FACE)
  message(FATAL_ERROR "Re-evaluate vision compile requirements before changing the pinned MSR/MNP s8 models")
endif()
idf_component_get_property(espbot_espdl_dir espressif__esp-dl COMPONENT_DIR)
idf_component_get_property(espbot_face_model_dir espressif__human_face_detect COMPONENT_DIR)
idf_build_get_property(espbot_python PYTHON)
set(espbot_vision_req "${CMAKE_BINARY_DIR}/vision.compile_req.yml")
execute_process(COMMAND "${espbot_python}"
  "${CMAKE_CURRENT_LIST_DIR}/../tools/vision_compile_requirements.py"
  --espdl "${espbot_espdl_dir}" --output "${espbot_vision_req}"
  --models "${espbot_face_model_dir}/models/p4"
  RESULT_VARIABLE espbot_req_result)
if(NOT espbot_req_result EQUAL 0)
  message(FATAL_ERROR "Could not derive vision compile requirements")
endif()
include("${espbot_espdl_dir}/cmake/compile_register.cmake")
esp_dl_compile_add_yml("${espbot_vision_req}")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS
  "${CMAKE_CURRENT_LIST_DIR}/../tools/vision_compile_requirements.py"
  "${espbot_face_model_dir}/models/p4/human_face_detect_msr_s8_v1.espdl"
  "${espbot_face_model_dir}/models/p4/human_face_detect_mnp_s8_v1.espdl"
  "${espbot_espdl_dir}/spec/ops.yml" "${espbot_espdl_dir}/spec/kernels.yml")
