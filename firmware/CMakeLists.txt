cmake_minimum_required(VERSION 3.16)
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
# Keep the complete official registry for this combined application until both
# upstream model components supply compatible compile_req sidecars.
idf_build_set_property(ESPDL_COMPILE_REQS "")
