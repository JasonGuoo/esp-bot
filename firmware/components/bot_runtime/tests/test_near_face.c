#include "near_face.h"
#include <assert.h>
#include <math.h>

static vatt_snapshot_t frame_at(uint32_t time, float width, float height)
{
    vatt_snapshot_t frame = {0};
    frame.timestamp_ms = time;
    frame.state = VATT_FACE_PRESENT;
    frame.face_count = 1;
    frame.faces[0].track_id = 1;
    frame.faces[0].visible = true;
    frame.faces[0].last_seen_ms = time;
    frame.faces[0].observation.face_confidence = 0.9f;
    frame.faces[0].observation.bbox = (vatt_box_t){0, 0, width, height};
    return frame;
}

int main(void)
{
    bot_near_face_t reaction = {0};
    uint16_t area = 999;
    vatt_snapshot_t frame = frame_at(1000, 0.5f, 0.4f);
    assert(!bot_near_face_step(&reaction, &frame, 1000, 350, 0.55f, true, &area));
    assert(area == 200); /* exactly 20% is not above the entry threshold */

    frame = frame_at(1020, 0.5f, 0.42f);
    assert(bot_near_face_step(&reaction, &frame, 1020, 350, 0.55f, true, &area));
    assert(area == 210);
    frame = frame_at(1040, 0.5f, 0.38f);
    assert(bot_near_face_step(&reaction, &frame, 1040, 350, 0.55f, true, &area));
    assert(area == 190); /* hysteresis while the detection fluctuates */
    frame = frame_at(1060, 0.5f, 0.34f);
    assert(bot_near_face_step(&reaction, &frame, 1060, 350, 0.55f, true, &area));
    assert(!bot_near_face_step(&reaction, &frame, 1441, 350, 0.55f, true, &area));

    frame = frame_at(2000, 0.5f, 0.5f);
    assert(bot_near_face_step(&reaction, &frame, 2000, 350, 0.55f, true, &area));
    assert(!bot_near_face_step(&reaction, &frame, 2001, 350, 0.55f, false, &area));
    assert(area == 0); /* unavailable camera/settings/fault clear immediately */
    assert(!bot_near_face_step(&reaction, &frame, 2351, 350, 0.55f, true, &area));
    assert(!bot_near_face_step(&reaction, &frame, 1999, 350, 0.55f, true, &area));

    frame = frame_at(3000, 0.4f, 0.4f);
    frame.face_count = 2;
    frame.faces[1] = frame.faces[0];
    frame.faces[1].track_id = 2;
    frame.faces[1].observation.bbox.right = 0.6f;
    assert(bot_near_face_step(&reaction, &frame, 3000, 350, 0.55f, true, &area));
    assert(area == 240); /* any fresh large face, not a fabricated identity */

    reaction = (bot_near_face_t){0};
    frame.faces[1].observation.bbox.right = NAN;
    assert(!bot_near_face_step(&reaction, &frame, 3000, 350, 0.55f, true, &area));
    frame.faces[1].observation.bbox.right = 0.6f;
    frame.faces[1].observation.face_confidence = 0.1f;
    assert(!bot_near_face_step(&reaction, &frame, 3000, 350, 0.55f, true, &area));
    frame.face_count = VATT_MAX_FACES + 1;
    assert(!bot_near_face_step(&reaction, &frame, 3000, 350, 0.55f, true, &area));

    frame = frame_at(UINT32_MAX - 100, 0.5f, 0.5f);
    assert(bot_near_face_step(&reaction, &frame, 50, 350, 0.55f, true, &area));
    assert(bot_near_face_step(&reaction, NULL, 450, 350, 0.55f, true, &area));
    assert(!bot_near_face_step(&reaction, NULL, 451, 350, 0.55f, true, &area));

    /* A delayed but still fresh inference gets a full grace from first receipt.
     * Rereading the same frame must not keep the smile alive indefinitely. */
    reaction = (bot_near_face_t){0};
    frame = frame_at(5000, 0.5f, 0.5f);
    assert(bot_near_face_step(&reaction, &frame, 5300, 350, 0.55f, true, &area));
    assert(bot_near_face_step(&reaction, &frame, 5350, 350, 0.55f, true, &area));
    assert(bot_near_face_step(&reaction, NULL, 5700, 350, 0.55f, true, &area));
    assert(!bot_near_face_step(&reaction, NULL, 5701, 350, 0.55f, true, &area));
    return 0;
}
