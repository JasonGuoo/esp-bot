/* EVE silhouettes from the user's reference. Uses LVGL's software line drawer;
 * no bitmap atlas, framebuffer, vector backend, timers or hardware ownership.
 * Both complete eyes translate with the same attention vector, without pupils.
 */
#include "face_renderer.h"
#include "eve_pose.h"
#include "eve_gaze.h"
#include <stdlib.h>

#define CURVE_STEPS 24
#define CONTOUR_POINTS (4 * CURVE_STEPS + 1)

typedef struct {
    lv_obj_t *root;
    lv_point_t contour[2][CONTOUR_POINTS];
    const face_theme_t *theme;
    int eye_height, eye_width, eye_cx[2], eye_cy;
    uint16_t glint;
    face_eve_pose_t pose;
} eve_t;

static int mini(int a, int b) { return a < b ? a : b; }
static int clamp(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }

static void contour(eve_t *r, int eye, const int16_t *shape,
                    int cx, int cy, int w, int h) {
    float x0 = shape[0], y0 = shape[1];
    r->contour[eye][0] = (lv_point_t){cx + (int)((x0 - 50) * w / 100),
                                                cy + (int)((y0 - 50) * h / 100)};
    for (int c = 0; c < 4; c++) {
        const int16_t *p = &shape[2 + c * 6];
        for (int k = 1; k <= CURVE_STEPS; k++) {
            const float t = (float)k / CURVE_STEPS, u = 1 - t;
            float x = u*u*u*x0 + 3*u*u*t*p[0] + 3*u*t*t*p[2] + t*t*t*p[4];
            float y = u*u*u*y0 + 3*u*u*t*p[1] + 3*u*t*t*p[3] + t*t*t*p[5];
            r->contour[eye][c * CURVE_STEPS + k] = (lv_point_t){
                cx + (int)((x - 50) * w / 100), cy + (int)((y - 50) * h / 100)};
        }
        x0 = p[4]; y0 = p[5];
    }
}

/* Even/odd scan conversion also preserves the concave smiling lower lid.
 * Four cubic curves have at most twelve intersections with a scan line.
 */
static int intersections(const lv_point_t *p, int y, int *xs) {
    int count = 0;
    for (int i = 0; i < CONTOUR_POINTS - 1; i++) {
        const lv_point_t a = p[i], b = p[i + 1];
        if (!((a.y <= y && b.y > y) || (b.y <= y && a.y > y))) continue;
        int x = a.x + (int)((int64_t)(y - a.y) * (b.x - a.x) / (b.y - a.y));
        int j = count;
        if (count == 12) break;
        while (j > 0 && xs[j - 1] > x) { xs[j] = xs[j - 1]; j--; }
        xs[j] = x;
        count++;
    }
    return count;
}

static void draw(lv_event_t *e) {
    eve_t *r = lv_event_get_user_data(e);
    if (!r->theme) return;
    lv_layer_t *layer = lv_event_get_layer(e);
    lv_area_t area;
    lv_obj_get_coords(r->root, &area);
    lv_draw_line_dsc_t line;
    lv_draw_line_dsc_init(&line);
    line.round_start = line.round_end = 1;
    const lv_color_t cyan = lv_color_hex(r->theme->eye);
    const int stripe = clamp(r->eye_height / 28, 2, 6);
    const int step = r->eye_height < 70 ? 1 : 2;
    for (int eye = 0; eye < 2; eye++) {
        int top = r->contour[eye][0].y, bottom = top;
        for (int i = 1; i < CONTOUR_POINTS; i++) {
            if (r->contour[eye][i].y < top) top = r->contour[eye][i].y;
            if (r->contour[eye][i].y > bottom) bottom = r->contour[eye][i].y;
        }
        /* Low-opacity wider spans make a bounded cyan halo. */
        for (int pass = 0; pass < 2; pass++) {
            const int stride = pass ? step : 4;
            line.width = pass ? step : clamp(r->eye_height / 12, 4, 12);
            line.opa = pass ? LV_OPA_COVER : LV_OPA_10;
            for (int y = top; y < bottom; y += stride) {
                int xs[12];
                const int count = intersections(r->contour[eye], y, xs);
                line.color = pass && (y - top) % stripe < stripe / 2 ?
                    lv_color_mix(lv_color_white(), cyan, 145) : cyan;
                for (int i = 0; i + 1 < count; i += 2) {
                    if (xs[i + 1] <= xs[i]) continue;
                    line.p1.x = area.x1 + xs[i];
                    line.p2.x = area.x1 + xs[i + 1];
                    line.p1.y = line.p2.y = area.y1 + y;
                    lv_draw_line(layer, &line);
                }
            }
        }
    }
    if (r->glint) {
        lv_draw_rect_dsc_t glint;
        lv_draw_rect_dsc_init(&glint);
        glint.bg_color = lv_color_white();
        glint.bg_opa = (lv_opa_t)(225u * r->glint / FACE_EVE_MIX_FULL);
        glint.radius = LV_RADIUS_CIRCLE;
        for (int eye = 0; eye < 2; eye++) {
            int x = area.x1 + r->eye_cx[eye] - r->eye_width * 19 / 100;
            int y = area.y1 + r->eye_cy - r->eye_height * 21 / 100;
            int w = clamp(r->eye_width * 8 / 100, 2, 100);
            int h = clamp(r->eye_height * 16 / 100, 3, 100);
            lv_area_t highlight = {x, y, x + w - 1, y + h - 1};
            lv_draw_rect(layer, &glint, &highlight);
            x += w + clamp(r->eye_width * 3 / 100, 1, 100);
            y += h * 3 / 4;
            w = clamp(r->eye_width * 3 / 100, 2, 100);
            h = clamp(r->eye_height * 5 / 100, 2, 100);
            highlight = (lv_area_t){x, y, x + w - 1, y + h - 1};
            lv_draw_rect(layer, &glint, &highlight);
        }
    }
}

static void *create(lv_obj_t *parent) {
    eve_t *r = calloc(1, sizeof(*r));
    if (!r) return NULL;
    r->root = lv_obj_create(parent);
    lv_obj_set_style_border_width(r->root, 0, 0);
    lv_obj_set_style_pad_all(r->root, 0, 0);
    lv_obj_set_style_radius(r->root, 0, 0);
    lv_obj_set_style_bg_opa(r->root, LV_OPA_COVER, 0);
    lv_obj_remove_flag(r->root, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_event_cb(r->root, draw, LV_EVENT_DRAW_MAIN, r);
    return r;
}

static void render(void *instance, const face_theme_t *theme,
                   const face_render_state_t *state, int width, int height) {
    eve_t *r = instance;
    r->theme = theme;
    lv_obj_set_pos(r->root, 0, 0);
    lv_obj_set_size(r->root, width, height);
    lv_obj_set_style_bg_color(r->root, lv_color_hex(theme->background), 0);
    int w = mini(width * 29 / 100, height * 50 / 100);
    int h = mini(height * 34 / 100, width * 26 / 100);
    h += h * state->speech_level / 2304; /* eyes open up to ~11% with the voice */
    /* Keep both large silhouettes inside the display while making a sideways
     * look clearly visible at normal viewing distance. */
    const int dx = face_eve_gaze_offset_x(width, state->gaze_x);
    const int dy = clamp(state->gaze_y, -100, 100) * height / 2800;
    face_expression_t ex = state->expression;
    if (ex < 0 || ex >= FACE_EX_COUNT) ex = FACE_EX_IDLE;
    const uint16_t blink = state->blinking && !state->blink_progress ?
        FACE_EVE_MIX_FULL : state->blink_progress;
    int16_t shapes[2][FACE_EVE_SHAPE_POINTS];
    face_eve_pose_step(&r->pose, ex, state->transition_active,
                       state->transition_progress, blink, shapes);
    r->eye_height = h;
    r->eye_width = w;
    r->eye_cy = height * 44 / 100 + dy;
    r->glint = face_eve_glint_strength(ex, state->transition_active,
                                       state->transition_progress, blink);
    for (int i = 0; i < 2; i++) {
        const int cx = width * (i ? 71 : 29) / 100 + dx;
        r->eye_cx[i] = cx;
        contour(r, i, shapes[i], cx, r->eye_cy, w, h);
    }
    lv_obj_invalidate(r->root);
}

static void destroy(void *instance) {
    eve_t *r = instance;
    if (!r) return;
    lv_obj_delete(r->root);
    free(r);
}

const face_renderer_driver_t face_eve_renderer_driver = {
    .id = "eve", .create = create, .render = render, .destroy = destroy,
};
