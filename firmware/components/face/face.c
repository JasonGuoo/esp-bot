/*
 * Face controller: expression model, animation timing, subtitle, settings.
 * No eye primitives belong here. Each theme chooses its own renderer.
 * This component does not import BSP, NVS or Brain networking.
 */
#include "face.h"
#include "face_renderer.h"
#include "face_settings.h"
#include "gaze_smoothing.h"
#include "fonts/xiaodai_font.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

struct face_view {
    lv_obj_t *root;
    lv_obj_t *eyes_host;
    face_renderer_t *eyes;
    lv_obj_t *caption_bar, *caption;
    lv_obj_t *gear_button, *gear_icon;
    lv_obj_t *stop_button, *stop_icon;
    face_settings_t *settings;
    const face_theme_t *theme;
    face_render_state_t state;
    lv_timer_t *timer;
    uint32_t blink_start, gear_expire;
    int last_layout_w, last_layout_h;
    int requested_gaze_x, requested_gaze_y;
    face_theme_applied_cb_t on_applied;
    void *on_applied_context;
    face_event_cb_t on_event;
    void *event_context;
    bool settings_was_open;
    bool animation_active;
    bool blink_anim_running;
};

static void emit(face_view_t *f, face_event_t event) {
    if (f && f->on_event) f->on_event(event, f->event_context);
}

static void sync_settings_event(face_view_t *f) {
    bool opened = face_settings_is_open(f->settings);
    if (opened == f->settings_was_open) return;
    f->settings_was_open = opened;
    emit(f, opened ? FACE_EVENT_SETTINGS_OPENED : FACE_EVENT_SETTINGS_CLOSED);
}

static void visible(lv_obj_t *o, bool show) {
    if (!o) return;
    if (show) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}

static int clamp(int v, int low, int high) {
    return v < low ? low : (v > high ? high : v);
}

static const face_theme_t *find_theme(const char *id) {
    if (!id) return NULL;
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        if (strcmp(k_face_themes[i].id, id) == 0) return &k_face_themes[i];
    }
    return NULL;
}

static void paint_chrome(face_view_t *f) {
    const face_theme_t *t = f->theme;
    lv_obj_set_style_bg_color(f->root, lv_color_hex(t->background), 0);
    lv_obj_set_style_bg_color(f->caption_bar, lv_color_hex(t->subtitle_bg), 0);
    lv_obj_set_style_text_color(f->caption, lv_color_hex(t->subtitle_text), 0);
    lv_obj_set_style_bg_color(f->gear_button, lv_color_hex(t->subtitle_bg), 0);
    lv_obj_set_style_text_color(f->gear_icon, lv_color_hex(t->chrome_text), 0);
    lv_obj_set_style_bg_color(f->stop_button, lv_color_hex(t->subtitle_bg), 0);
    lv_obj_set_style_text_color(f->stop_icon, lv_color_hex(t->chrome_text), 0);
    if (f->settings) face_settings_set_active_theme(f->settings, t->id);
}

static void draw_face(face_view_t *f) {
    if (!f || !f->eyes) return;
    const int w = lv_obj_get_width(f->root);
    const int h = lv_obj_get_height(f->root);
    if (w < 240 || h < 180) return;
    lv_obj_set_size(f->eyes_host, w, h);
    lv_obj_set_pos(f->eyes_host, 0, 0);
    face_renderer_render(f->eyes, f->theme, &f->state, w, h);

    int text_h = h * 13 / 100;
    if (text_h > 62) text_h = 62;
    const int cap_y = h * 83 / 100;
    lv_obj_set_pos(f->caption_bar, w * 7 / 100, cap_y);
    lv_obj_set_size(f->caption_bar, w * 86 / 100, text_h);
    lv_obj_set_pos(f->caption, w * 9 / 100, cap_y + 7);
    lv_obj_set_size(f->caption, w * 82 / 100, text_h - 12);
    lv_obj_align(f->gear_button, LV_ALIGN_TOP_RIGHT, -24, 22);
    lv_obj_align(f->stop_button, LV_ALIGN_TOP_LEFT, 24, 22);
    if (f->settings && (w != f->last_layout_w || h != f->last_layout_h)) {
        face_settings_layout(f->settings, w, h);
        f->last_layout_w = w;
        f->last_layout_h = h;
    }
}

/* Use LVGL's animation engine for easing; the EVE renderer interpolates its
 * existing eye contours and can continue smoothly if a new pose interrupts. */
static void expression_anim_step(void *context, int32_t value) {
    face_view_t *f = context;
    f->state.transition_progress = (uint16_t)clamp(value, 0, 256);
    f->state.transition_active = value < 256;
    draw_face(f);
}

static void blink_anim_step(void *context, int32_t value) {
    face_view_t *f = context;
    f->state.blink_progress = (uint16_t)clamp(value, 0, 256);
    f->state.blinking = value > 0;
    draw_face(f);
}

static void blink_anim_completed(lv_anim_t *animation) {
    face_view_t *f = animation->var;
    const bool redraw = f->state.blinking || f->state.blink_progress != 0;
    f->blink_anim_running = false;
    f->state.blinking = false;
    f->state.blink_progress = 0;
    if (redraw) draw_face(f);
}

static bool expression_can_blink(face_expression_t expression) {
    return expression != FACE_EX_SLEEPING && expression != FACE_EX_SLEEPY &&
           expression != FACE_EX_DROWSY && expression != FACE_EX_BLINK &&
           expression != FACE_EX_WINK;
}

static void start_blink(face_view_t *f, uint32_t now) {
    lv_anim_t animation;
    lv_anim_init(&animation);
    lv_anim_set_var(&animation, f);
    lv_anim_set_values(&animation, 0, 256);
    lv_anim_set_duration(&animation, f->theme->blink_duration_ms * 2 / 5);
    lv_anim_set_reverse_duration(&animation, f->theme->blink_duration_ms * 3 / 5);
    lv_anim_set_reverse_delay(&animation, 20);
    lv_anim_set_path_cb(&animation, lv_anim_path_ease_in_out);
    lv_anim_set_exec_cb(&animation, blink_anim_step);
    lv_anim_set_completed_cb(&animation, blink_anim_completed);
    f->blink_start = now;
    f->blink_anim_running = true;
    if (!lv_anim_start(&animation)) f->blink_anim_running = false;
}

static bool applied_from_settings(const char *id, void *context) {
    face_view_t *f = context;
    if (!f || !face_set_theme(f, id)) return false;
    if (f->on_applied) f->on_applied(id, f->on_applied_context);
    visible(f->gear_button, false);
    return true;
}

static void open_settings(lv_event_t *event) {
    face_view_t *f = lv_event_get_user_data(event);
    if (!f || !f->settings) return;
    visible(f->gear_button, false);
    face_settings_open(f->settings, f->theme->id);
    sync_settings_event(f);
}

static void tap_face(lv_event_t *event) {
    face_view_t *f = lv_event_get_user_data(event);
    if (f && !face_settings_is_open(f->settings)) {
        face_reveal_settings(f);
        emit(f, FACE_EVENT_TOUCH);
    }
}

static void stop_pressed(lv_event_t *event) {
    emit(lv_event_get_user_data(event), FACE_EVENT_STOP);
}

static void animation_tick(lv_timer_t *timer) {
    face_view_t *f = lv_timer_get_user_data(timer);
    const uint32_t now = lv_tick_get();
    bool redraw = false;
    sync_settings_event(f);

    const bool auto_blink = expression_can_blink(f->state.expression);
    if (auto_blink && !f->blink_anim_running &&
        (uint32_t)(now - f->blink_start) >= f->theme->blink_interval_ms) {
        start_blink(f, now);
    }

    const int factor = clamp(f->theme->gaze_smoothing, 1, 20);
    int dx = f->requested_gaze_x - f->state.gaze_x;
    int dy = f->requested_gaze_y - f->state.gaze_y;
    if (dx) {
        f->state.gaze_x = face_gaze_approach(f->state.gaze_x, f->requested_gaze_x, factor);
        redraw = true;
    }
    if (dy) {
        f->state.gaze_y = face_gaze_approach(f->state.gaze_y, f->requested_gaze_y, factor);
        redraw = true;
    }
    if (redraw) draw_face(f);

    if (!face_settings_is_open(f->settings) &&
        !lv_obj_has_flag(f->gear_button, LV_OBJ_FLAG_HIDDEN) &&
        (int32_t)(now - f->gear_expire) >= 0) {
        visible(f->gear_button, false);
    }
}

face_view_t *face_create(lv_obj_t *parent) {
    if (!parent) return NULL;
    face_view_t *f = calloc(1, sizeof(*f));
    if (!f) return NULL;
    f->theme = &k_face_themes[FACE_DEFAULT_THEME_INDEX];
    f->state.expression = FACE_EX_IDLE;
    f->blink_start = lv_tick_get();

    f->root = lv_obj_create(parent);
    lv_obj_set_size(f->root, lv_pct(100), lv_pct(100));
    lv_obj_set_style_pad_all(f->root, 0, 0);
    lv_obj_set_style_border_width(f->root, 0, 0);
    lv_obj_set_style_radius(f->root, 0, 0);
    lv_obj_remove_flag(f->root, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(f->root, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_cb(f->root, tap_face, LV_EVENT_CLICKED, f);

    /* Owns whatever children its installed renderer creates. */
    f->eyes_host = lv_obj_create(f->root);
    lv_obj_set_style_bg_opa(f->eyes_host, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(f->eyes_host, 0, 0);
    lv_obj_set_style_pad_all(f->eyes_host, 0, 0);
    lv_obj_remove_flag(f->eyes_host,
                       LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    f->eyes = face_renderer_create(f->theme->renderer_id, f->eyes_host);
    if (!f->eyes) {
        lv_obj_delete(f->root);
        free(f);
        return NULL;
    }

    f->caption_bar = lv_obj_create(f->root);
    lv_obj_set_style_radius(f->caption_bar, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_pad_all(f->caption_bar, 0, 0);
    lv_obj_set_style_border_width(f->caption_bar, 0, 0);
    lv_obj_remove_flag(f->caption_bar,
                       LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    f->caption = lv_label_create(f->root);
    lv_label_set_long_mode(f->caption, LV_LABEL_LONG_MODE_DOTS);
    lv_label_set_text(f->caption, "");
    lv_obj_set_style_text_align(f->caption, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_set_style_text_font(f->caption, &xiaodai_font_sc_16, 0);
    visible(f->caption_bar, false);
    visible(f->caption, false);

    f->gear_button = lv_button_create(f->root);
    lv_obj_set_size(f->gear_button, 46, 46);
    lv_obj_set_style_radius(f->gear_button, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_border_width(f->gear_button, 0, 0);
    lv_obj_set_style_shadow_width(f->gear_button, 0, 0);
    lv_obj_add_event_cb(f->gear_button, open_settings, LV_EVENT_CLICKED, f);
    f->gear_icon = lv_label_create(f->gear_button);
    lv_label_set_text(f->gear_icon, LV_SYMBOL_SETTINGS);
    lv_obj_center(f->gear_icon);
    visible(f->gear_button, false);

    f->stop_button = lv_button_create(f->root);
    lv_obj_set_size(f->stop_button, 54, 46);
    lv_obj_set_style_radius(f->stop_button, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_border_width(f->stop_button, 0, 0);
    lv_obj_set_style_shadow_width(f->stop_button, 0, 0);
    lv_obj_add_event_cb(f->stop_button, stop_pressed, LV_EVENT_PRESSED, f);
    f->stop_icon = lv_label_create(f->stop_button);
    lv_label_set_text(f->stop_icon, LV_SYMBOL_STOP);
    lv_obj_center(f->stop_icon);
    visible(f->stop_button, false);

    f->settings = face_settings_create(f->root, applied_from_settings, f);
    if (!f->settings) {
        face_renderer_destroy(f->eyes);
        lv_obj_delete(f->root);
        free(f);
        return NULL;
    }
    lv_obj_update_layout(f->root);
    paint_chrome(f);
    draw_face(f);
    f->timer = lv_timer_create(animation_tick, 33, f);
    f->animation_active = true;
    return f;
}

void face_destroy(face_view_t *f) {
    if (!f) return;
    lv_anim_delete(f, expression_anim_step);
    lv_anim_delete(f, blink_anim_step);
    if (f->timer) lv_timer_delete(f->timer);
    if (f->settings) face_settings_destroy(f->settings);
    if (f->eyes) face_renderer_destroy(f->eyes);
    if (f->root) lv_obj_delete(f->root);
    free(f);
}

bool face_set_theme(face_view_t *f, const char *id) {
    if (!f) return false;
    const face_theme_t *theme = find_theme(id);
    if (!theme) return false;

    /* Switch the actual renderer implementation if the theme requires it.
       Not just a recolor: this permits different geometry, assets, effects. */
    if (strcmp(face_renderer_id(f->eyes), theme->renderer_id) != 0) {
        face_renderer_t *new_renderer =
            face_renderer_create(theme->renderer_id, f->eyes_host);
        if (!new_renderer) return false;
        face_renderer_destroy(f->eyes);
        f->eyes = new_renderer;
    }
    f->theme = theme;
    paint_chrome(f);
    draw_face(f);
    return true;
}

bool face_set_expression(face_view_t *f, const char *name) {
    if (!f || !name) return false;
    for (size_t i = 0; i < FACE_EX_COUNT; i++) {
        if (strcmp(k_face_expression_ids[i], name) == 0) {
            if (f->state.expression == (face_expression_t)i) return true;
            lv_anim_delete(f, expression_anim_step);
            f->state.expression = (face_expression_t)i;
            if (!expression_can_blink(f->state.expression)) {
                lv_anim_delete(f, blink_anim_step);
                f->blink_anim_running = false;
                f->state.blinking = false;
                f->state.blink_progress = 0;
            }
            f->state.transition_progress = 0;
            f->state.transition_active = f->animation_active &&
                strcmp(face_renderer_id(f->eyes), "eve") == 0;
            /* Capture the renderer's current intermediate contour before the
             * LVGL animation advances, including interrupted transitions. */
            draw_face(f);
            if (f->state.transition_active) {
                lv_anim_t animation;
                lv_anim_init(&animation);
                lv_anim_set_var(&animation, f);
                lv_anim_set_values(&animation, 0, 256);
                const bool quick = f->state.expression == FACE_EX_SURPRISED ||
                                   f->state.expression == FACE_EX_DISCOVERY ||
                                   f->state.expression == FACE_EX_WINK;
                const bool sleepy = f->state.expression == FACE_EX_DROWSY ||
                                    f->state.expression == FACE_EX_SLEEPING ||
                                    f->state.expression == FACE_EX_SLEEPY;
                lv_anim_set_duration(&animation, sleepy ? 420 : quick ? 180 : 280);
                lv_anim_set_path_cb(&animation, lv_anim_path_ease_in_out);
                lv_anim_set_exec_cb(&animation, expression_anim_step);
                if (!lv_anim_start(&animation)) {
                    f->state.transition_active = false;
                    draw_face(f);
                }
            }
            return true;
        }
    }
    return false;
}

void face_set_gaze(face_view_t *f, int x, int y) {
    if (!f) return;
    f->requested_gaze_x = clamp(x, -100, 100);
    f->requested_gaze_y = clamp(y, -100, 100);
}

void face_get_gaze(const face_view_t *f, int *x, int *y) {
    if (!f) return;
    if (x) *x = f->state.gaze_x;
    if (y) *y = f->state.gaze_y;
}

void face_set_subtitle(face_view_t *f, const char *text) {
    if (!f) return;
    if (!text || !*text) {
        lv_label_set_text(f->caption, "");
        visible(f->caption, false);
        visible(f->caption_bar, false);
        return;
    }
    char buffer[384];
    size_t n = strlen(text);
    if (n >= sizeof(buffer)) n = sizeof(buffer) - 1;
    while (n > 0 && (((unsigned char)text[n]) & 0xC0) == 0x80) n--;
    memcpy(buffer, text, n);
    buffer[n] = '\0';
    lv_label_set_text(f->caption, buffer);
    visible(f->caption, true);
    visible(f->caption_bar, true);
}

void face_reveal_settings(face_view_t *f) {
    if (!f || face_settings_is_open(f->settings)) return;
    visible(f->gear_button, true);
    f->gear_expire = lv_tick_get() + 5000;
}

void face_on_theme_applied(face_view_t *f,
                           face_theme_applied_cb_t callback, void *context) {
    if (!f) return;
    f->on_applied = callback;
    f->on_applied_context = context;
}

void face_on_event(face_view_t *f, face_event_cb_t callback, void *context) {
    if (!f) return;
    f->on_event = callback;
    f->event_context = context;
}
void face_set_stop_visible(face_view_t *f, bool show) {
    if (f) visible(f->stop_button, show);
}
void face_set_animation_active(face_view_t *f, bool active) {
    if (!f || !f->timer || f->animation_active == active) return;
    f->animation_active = active;
    if (active) {
        f->blink_start = lv_tick_get();
        lv_timer_resume(f->timer);
        lv_timer_reset(f->timer);
        draw_face(f);
    } else {
        lv_anim_delete(f, expression_anim_step);
        lv_anim_delete(f, blink_anim_step);
        f->state.transition_active = false;
        f->state.transition_progress = 256;
        f->state.blinking = false;
        f->state.blink_progress = 0;
        f->blink_anim_running = false;
        lv_timer_pause(f->timer);
    }
}

const char *face_theme_id(const face_view_t *f) {
    return f ? f->theme->id : NULL;
}
const char *face_expression_id(const face_view_t *f) {
    return f ? k_face_expression_ids[f->state.expression] : NULL;
}
lv_obj_t *face_root(face_view_t *f) { return f ? f->root : NULL; }
