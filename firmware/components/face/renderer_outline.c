/* Original macaroon line-art eyes. No UI navigation or expression state machine. */
#include "face_renderer.h"
#include <stdlib.h>

typedef enum { MODE_OPEN, MODE_ARCH, MODE_SLIT } eye_mode_t;
typedef struct {
    lv_obj_t *root;
    lv_obj_t *outer[2], *inner[2], *pupil[2], *mask[2], *slit[2];
    lv_obj_t *mouth;
} outline_t;

static int mini(int a, int b) { return a < b ? a : b; }
static int clamp(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }
static void show(lv_obj_t *o, bool enabled) {
    if (enabled) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}
static void pos(lv_obj_t *o, int x, int y, int w, int h) {
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
}
static lv_obj_t *shape(lv_obj_t *parent) {
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_set_style_radius(o, LV_RADIUS_CIRCLE, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    return o;
}

static void *create(lv_obj_t *parent) {
    outline_t *r = calloc(1, sizeof(*r));
    if (!r) return NULL;
    r->root = lv_obj_create(parent);
    lv_obj_set_style_bg_opa(r->root, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(r->root, 0, 0);
    lv_obj_set_style_pad_all(r->root, 0, 0);
    lv_obj_remove_flag(r->root, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    for (int i = 0; i < 2; i++) {
        r->outer[i] = shape(r->root);
        r->inner[i] = shape(r->root);
        r->pupil[i] = shape(r->root);
        r->mask[i] = shape(r->root); /* cover lower half for smiling arch */
        r->slit[i] = shape(r->root);
    }
    r->mouth = shape(r->root);
    return r;
}

static void eye(outline_t *r, int i, int cx, int cy, int w, int h,
                eye_mode_t mode, int dx, int dy, int stroke) {
    const bool slit = mode == MODE_SLIT, arch = mode == MODE_ARCH;
    show(r->outer[i], !slit);
    show(r->inner[i], !slit);
    show(r->pupil[i], !slit && !arch);
    show(r->mask[i], arch);
    show(r->slit[i], slit);
    if (slit) {
        const int line_w = w * 74 / 100;
        pos(r->slit[i], cx - line_w / 2, cy + stroke / 3,
            line_w, mini(15, stroke));
        return;
    }
    pos(r->outer[i], cx - w / 2, cy - h / 2, w, h);
    pos(r->inner[i], cx - w / 2 + stroke, cy - h / 2 + stroke,
        w - 2 * stroke, h - 2 * stroke);
    if (arch) {
        pos(r->mask[i], cx - w / 2 - 2, cy + stroke / 4,
            w + 4, h / 2 + stroke);
    } else {
        const int radius = clamp(stroke / 2, 2, 12);
        pos(r->pupil[i], cx + dx - radius, cy + dy - radius,
            radius * 2, radius * 2);
    }
}

static void render(void *instance, const face_theme_t *theme,
                   const face_render_state_t *state, int width, int height) {
    outline_t *r = instance;
    lv_obj_set_size(r->root, width, height);
    lv_obj_set_pos(r->root, 0, 0);
    lv_color_t fg = lv_color_hex(theme->eye), bg = lv_color_hex(theme->background);
    lv_obj_set_style_bg_color(r->root, bg, 0);
    lv_obj_set_style_bg_opa(r->root, LV_OPA_COVER, 0);
    for (int i = 0; i < 2; i++) {
        lv_obj_set_style_bg_color(r->outer[i], fg, 0);
        lv_obj_set_style_bg_color(r->inner[i], bg, 0);
        lv_obj_set_style_bg_color(r->pupil[i], lv_color_hex(theme->pupil), 0);
        lv_obj_set_style_bg_color(r->mask[i], bg, 0);
        lv_obj_set_style_bg_color(r->slit[i], fg, 0);
    }
    lv_obj_set_style_bg_color(r->mouth, fg, 0);
    int ew = mini(width * 21 / 100, height * 36 / 100);
    int eh = mini(height * 31 / 100, width * 25 / 100);
    int stroke = clamp(mini(ew, eh) / 7, 3, 23);
    int cx1 = width * 32 / 100, cx2 = width * 68 / 100, cy = height * 44 / 100;
    int rw = ew, rh = eh, ry = cy, lw = ew, lh = eh;
    eye_mode_t left = MODE_OPEN, right = MODE_OPEN;
    switch (state->expression) {
        case FACE_EX_HAPPY:
        case FACE_EX_SHY: left = MODE_ARCH; right = MODE_ARCH; break;
        case FACE_EX_SLEEPY:
        case FACE_EX_BLINK: left = MODE_SLIT; right = MODE_SLIT; break;
        case FACE_EX_CURIOUS: rw = ew * 82 / 100; rh = eh * 76 / 100; ry = cy + eh / 13; break;
        case FACE_EX_THINKING: right = MODE_SLIT; break;
        case FACE_EX_SURPRISED:
            lw = ew * 110 / 100; rw = lw;
            lh = eh * 110 / 100; rh = lh;
            break;
        default: break;
    }
    if (state->blinking &&
        (state->expression == FACE_EX_IDLE || state->expression == FACE_EX_LISTENING)) {
        left = right = MODE_SLIT;
    }
    const int dx = state->gaze_x * ew / 630;
    const int dy = state->gaze_y * eh / 630;
    eye(r, 0, cx1, cy, lw, lh, left, dx, dy, stroke);
    eye(r, 1, cx2, ry, rw, rh, right, dx, dy, stroke);
    bool surprised = state->expression == FACE_EX_SURPRISED;
    bool shy = state->expression == FACE_EX_SHY;
    show(r->mouth, surprised || shy);
    int mw = surprised ? stroke : stroke * 2;
    int mh = surprised ? stroke : mini(7, stroke);
    if (surprised || shy) {
        pos(r->mouth, width / 2 - mw / 2, cy + eh / 2 + height * 5 / 100, mw, mh);
    }
}

static void destroy(void *instance) {
    outline_t *r = instance;
    if (!r) return;
    lv_obj_delete(r->root);
    free(r);
}

const face_renderer_driver_t face_outline_renderer_driver = {
    .id = "outline",
    .create = create,
    .render = render,
    .destroy = destroy,
};
