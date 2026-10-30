#pragma once
#include "face.h"

/** Start optional Wi-Fi command polling; returns immediately.
 *  Safe to omit: the face works offline without Brain. */
void brain_client_start(face_view_t *face);
