/*
 * Alternative complete eye-style driver: solid rounded capsules instead of
 * hollow outlined pupils. Kept compiled as a proof that a theme can swap the
 * rendering engine, not merely recolor pixels. Can be selected by a future
 * appearance manifest with "renderer": "pill".
 */
#include "face_renderer.h"
#include <stdlib.h>

typedef struct {
    lv_obj_t *root;
    lv_obj_t *eye[2];
    lv_obj_t *mouth;
} pill_t;

static void set_visible(lv_obj_t *o, bool yes) {
    if (yes) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}
static void place(lv_obj_t *o, int x, int y, int w, int h) {
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
}
static lv_obj_t *shape(lv_obj_t *parent) {
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_radius(o, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    return o;
}
static void *create(lv_obj_t *parent) {
    pill_t *r = calloc(1, sizeof(*r));
    if (!r) return NULL;
    r->root = shape(parent);
    r->eye[0] = shape(r->root);
    r->eye[1] = shape(r->root);
    r->mouth = shape(r->root);
    return r;
}
static void render(void *instance, const face_theme_t *theme,
                   const face_render_state_t *state, int width, int height) {
    pill_t *r = instance;
    place(r->root, 0, 0, width, height);
    lv_obj_set_style_bg_color(r->root, lv_color_hex(theme->background), 0);
    const int ew = width * 14 / 100, eh = height * 24 / 100;
    const int cy = height * 43 / 100;
    for (int i = 0; i < 2; i++) {
        int w = ew, h = eh;
        int x = width * (i ? 68 : 32) / 100 - w / 2;
        int y = cy - h / 2;
        switch (state->expression) {
            case FACE_EX_HAPPY: h = height * 7 / 100; y += eh / 4; break;
            case FACE_EX_SLEEPY:
            case FACE_EX_BLINK: h = height * 5 / 100; y += eh / 4; break;
            case FACE_EX_CURIOUS: if (i == 1) h = eh * 60 / 100; break;
            case FACE_EX_THINKING: if (i == 1) h = height * 6 / 100; break;
            case FACE_EX_SURPRISED: w += width * 2 / 100; h += height * 4 / 100; break;
            case FACE_EX_SHY: h = height * 8 / 100; break;
            default: break;
        }
        if (state->blinking || state->expression == FACE_EX_BLINK) h = height * 5 / 100;
        x = width * (i ? 68 : 32) / 100 - w / 2 + state->gaze_x * width / 1700;
        y += state->gaze_y * height / 1700;
        place(r->eye[i], x, y, w, h);
        lv_obj_set_style_bg_color(r->eye[i], lv_color_hex(theme->eye), 0);
    }
    bool mouth = state->expression == FACE_EX_SURPRISED || state->expression == FACE_EX_SHY;
    set_visible(r->mouth, mouth);
    if (mouth) {
        int d = state->expression == FACE_EX_SURPRISED ? height * 3 / 100 : width * 3 / 100;
        int h = state->expression == FACE_EX_SURPRISED ? d : height / 100;
        place(r->mouth, width / 2 - d / 2, cy + eh / 2 + height / 20, d, h);
        lv_obj_set_style_bg_color(r->mouth, lv_color_hex(theme->eye), 0);
    }
}
static void destroy(void *instance) {
    pill_t *r = instance;
    if (!r) return;
    lv_obj_delete(r->root);
    free(r);
}
const face_renderer_driver_t face_pill_renderer_driver = {
    .id = "pill", .create = create, .render = render, .destroy = destroy,
};
