#include "face.h"
#include "face_themes_generated.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Simple rounded LVGL objects: no bitmap sprites, realistic iris or gradients.
   BSP-independent: the application provides display initialization and locking. */

static const char *const EXPRESSIONS[] = {
    "idle", "listening", "happy", "curious", "thinking",
    "surprised", "sleepy", "shy", "blink"
};
enum { IDLE, LISTENING, HAPPY, CURIOUS, THINKING, SURPRISED, SLEEPY, SHY, BLINK };

typedef enum { EYE_OPEN, EYE_ARCH, EYE_SLIT } eye_mode_t;

struct face_view {
    lv_obj_t *root;
    lv_obj_t *outer[2], *inner[2], *dot[2], *lower_mask[2], *slit[2];
    lv_obj_t *mouth, *caption_bar, *caption;
    lv_obj_t *settings_button, *settings_panel, *settings_label, *panel_title;
    lv_obj_t *theme_buttons[5];
    lv_timer_t *tick;
    const face_theme_palette_t *palette;
    int expression, gaze_x, gaze_y, target_x, target_y;
    uint32_t last_blink, blink_until, settings_until;
    bool blinking;
};

static int minimum(int a, int b) { return a < b ? a : b; }
static int clamp(int v, int low, int high) {
    return v < low ? low : (v > high ? high : v);
}
static void show(lv_obj_t *o, bool visible) {
    if (!o) return;
    if (visible) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}
static void put(lv_obj_t *o, int x, int y, int w, int h) {
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
}

static lv_obj_t *shape(lv_obj_t *parent) {
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_set_style_radius(o, LV_RADIUS_CIRCLE, 0);
    return o;
}

static void eye_layout(face_view_t *f, int idx, int cx, int cy,
                       int w, int h, eye_mode_t mode) {
    int x = cx - w / 2, y = cy - h / 2;
    bool slit = mode == EYE_SLIT, arch = mode == EYE_ARCH;
    show(f->outer[idx], !slit);
    show(f->inner[idx], !slit);
    show(f->dot[idx], !slit && !arch);
    show(f->lower_mask[idx], arch);
    show(f->slit[idx], slit);
    if (slit) {
        int line_w = w * 74 / 100;
        put(f->slit[idx], cx - line_w / 2, cy + 3, line_w, 15);
        return;
    }
    put(f->outer[idx], x, y, w, h);
    int border = clamp(minimum(w, h) / 7, 14, 23);
    put(f->inner[idx], x + border, y + border, w - 2 * border, h - 2 * border);
    if (arch) {
        /* Cover lower half of a hollow ellipse to leave a clean U-inverted arc. */
        put(f->lower_mask[idx], x - 2, cy + 3, w + 4, h / 2 + 8);
    } else {
        int px = cx + (f->gaze_x * w / 630);
        int py = cy + (f->gaze_y * h / 630);
        put(f->dot[idx], px - 11, py - 11, 22, 22);
    }
}

static void layout(face_view_t *f) {
    if (!f || !f->root) return;
    int w = lv_obj_get_width(f->root);
    int h = lv_obj_get_height(f->root);
    if (w < 240 || h < 180) return;
    int ew = minimum(w * 21 / 100, 172);
    int eh = minimum(h * 31 / 100, 152);
    int cx1 = w * 32 / 100, cx2 = w * 68 / 100, cy = h * 44 / 100;
    eye_mode_t left = EYE_OPEN, right = EYE_OPEN;
    int lw = ew, rw = ew, lh = eh, rh = eh, ry = cy;

    switch (f->expression) {
        case HAPPY:
        case SHY: left = EYE_ARCH; right = EYE_ARCH; break;
        case SLEEPY:
        case BLINK: left = EYE_SLIT; right = EYE_SLIT; break;
        case CURIOUS: rw = ew * 82 / 100; rh = eh * 76 / 100;
                      ry = cy + eh / 13; break;
        case THINKING: right = EYE_SLIT; break;
        case SURPRISED: lw = ew * 110 / 100; rw = lw;
                        lh = eh * 110 / 100; rh = lh; break;
        default: break;
    }
    if (f->blinking && (f->expression == IDLE || f->expression == LISTENING)) {
        left = EYE_SLIT;
        right = EYE_SLIT;
    }
    eye_layout(f, 0, cx1, cy, lw, lh, left);
    eye_layout(f, 1, cx2, ry, rw, rh, right);

    bool dot_mouth = f->expression == SURPRISED;
    bool shy_mouth = f->expression == SHY;
    show(f->mouth, dot_mouth || shy_mouth);
    if (dot_mouth) put(f->mouth, w / 2 - 8, cy + eh / 2 + 25, 16, 16);
    else if (shy_mouth) put(f->mouth, w / 2 - 13, cy + eh / 2 + 27, 26, 7);

    int caption_h = minimum(h * 13 / 100, 62);
    int cap_y = h * 83 / 100;
    put(f->caption_bar, w * 7 / 100, cap_y, w * 86 / 100, caption_h);
    put(f->caption, w * 9 / 100, cap_y + 6, w * 82 / 100, caption_h - 10);
    lv_obj_align(f->settings_button, LV_ALIGN_TOP_RIGHT, -22, 20);
    lv_obj_align(f->settings_panel, LV_ALIGN_TOP_RIGHT, -20, 72);
}

static void repaint(face_view_t *f) {
    const face_theme_palette_t *p = f->palette;
    lv_obj_set_style_bg_color(f->root, lv_color_hex(p->background), 0);
    for (int i = 0; i < 2; ++i) {
        lv_obj_set_style_bg_color(f->outer[i], lv_color_hex(p->eye), 0);
        lv_obj_set_style_bg_color(f->inner[i], lv_color_hex(p->background), 0);
        lv_obj_set_style_bg_color(f->dot[i], lv_color_hex(p->pupil), 0);
        lv_obj_set_style_bg_color(f->lower_mask[i], lv_color_hex(p->background), 0);
        lv_obj_set_style_bg_color(f->slit[i], lv_color_hex(p->eye), 0);
    }
    lv_obj_set_style_bg_color(f->mouth, lv_color_hex(p->eye), 0);
    lv_obj_set_style_bg_color(f->caption_bar, lv_color_hex(p->subtitle_bg), 0);
    lv_obj_set_style_text_color(f->caption, lv_color_hex(p->subtitle_text), 0);
    lv_obj_set_style_bg_color(f->settings_button, lv_color_hex(p->subtitle_bg), 0);
    lv_obj_set_style_text_color(f->settings_label, lv_color_hex(p->chrome_text), 0);
    lv_obj_set_style_bg_color(f->settings_panel, lv_color_hex(p->subtitle_bg), 0);
    lv_obj_set_style_text_color(f->panel_title, lv_color_hex(p->subtitle_text), 0);
    for (size_t i = 0; i < FACE_THEME_COUNT && i < 5; ++i) {
        lv_obj_set_style_bg_color(f->theme_buttons[i],
                                  lv_color_hex(p->background), 0);
        lv_obj_t *label = lv_obj_get_child(f->theme_buttons[i], 0);
        lv_obj_set_style_text_color(label, lv_color_hex(p->subtitle_text), 0);
    }
}

static void theme_click(lv_event_t *e) {
    face_view_t *f = (face_view_t *)lv_event_get_user_data(e);
    lv_obj_t *button = lv_event_get_target_obj(e);
    for (size_t i = 0; i < FACE_THEME_COUNT && i < 5; i++) {
        if (f->theme_buttons[i] == button) {
            face_set_theme(f, k_face_palettes[i].id);
            show(f->settings_panel, false);
            face_reveal_settings(f);
            return;
        }
    }
}

static void gear_click(lv_event_t *e) {
    face_view_t *f = (face_view_t *)lv_event_get_user_data(e);
    show(f->settings_panel, lv_obj_has_flag(f->settings_panel, LV_OBJ_FLAG_HIDDEN));
    face_reveal_settings(f);
}

static void tap(lv_event_t *e) {
    face_view_t *f = (face_view_t *)lv_event_get_user_data(e);
    face_reveal_settings(f);
}

static void tick(lv_timer_t *timer) {
    face_view_t *f = (face_view_t *)lv_timer_get_user_data(timer);
    uint32_t now = lv_tick_get();
    bool changed = false;
    if (!f->blinking && (f->expression == IDLE || f->expression == LISTENING)
        && (uint32_t)(now - f->last_blink) > 3600) {
        f->blinking = true;
        f->blink_until = now + 130;
        f->last_blink = now;
        changed = true;
    }
    if (f->blinking && (int32_t)(now - f->blink_until) >= 0) {
        f->blinking = false;
        changed = true;
    }
    int dx = f->target_x - f->gaze_x;
    int dy = f->target_y - f->gaze_y;
    if (dx || dy) {
        f->gaze_x += dx == 0 ? 0 : (dx > 0 ? (dx + 3) / 4 : (dx - 3) / 4);
        f->gaze_y += dy == 0 ? 0 : (dy > 0 ? (dy + 3) / 4 : (dy - 3) / 4);
        changed = true;
    }
    if (lv_obj_has_flag(f->settings_panel, LV_OBJ_FLAG_HIDDEN)
        && !lv_obj_has_flag(f->settings_button, LV_OBJ_FLAG_HIDDEN)
        && (int32_t)(now - f->settings_until) >= 0) {
        show(f->settings_button, false);
    }
    if (changed) layout(f);
}

face_view_t *face_create(lv_obj_t *parent) {
    if (!parent) return NULL;
    face_view_t *f = (face_view_t *)calloc(1, sizeof(*f));
    if (!f) return NULL;
    f->palette = &k_face_palettes[0];
    f->expression = IDLE;
    f->last_blink = lv_tick_get();

    f->root = lv_obj_create(parent);
    lv_obj_set_size(f->root, lv_pct(100), lv_pct(100));
    lv_obj_center(f->root);
    lv_obj_set_style_pad_all(f->root, 0, 0);
    lv_obj_set_style_border_width(f->root, 0, 0);
    lv_obj_set_style_radius(f->root, 0, 0);
    lv_obj_remove_flag(f->root, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(f->root, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_cb(f->root, tap, LV_EVENT_PRESSED, f);

    for (int i = 0; i < 2; ++i) {
        f->outer[i] = shape(f->root);
        f->inner[i] = shape(f->root);
        f->dot[i] = shape(f->root);
        f->lower_mask[i] = shape(f->root);
        f->slit[i] = shape(f->root);
    }
    f->mouth = shape(f->root);
    f->caption_bar = shape(f->root);
    lv_obj_set_style_radius(f->caption_bar, 25, 0);
    f->caption = lv_label_create(f->root);
    lv_label_set_long_mode(f->caption, LV_LABEL_LONG_MODE_DOTS);
    lv_label_set_text(f->caption, "");
    lv_obj_set_style_text_align(f->caption, LV_TEXT_ALIGN_CENTER, 0);
    show(f->caption_bar, false);
    show(f->caption, false);

    f->settings_button = lv_button_create(f->root);
    lv_obj_set_size(f->settings_button, 44, 44);
    lv_obj_set_style_radius(f->settings_button, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_border_width(f->settings_button, 0, 0);
    lv_obj_add_event_cb(f->settings_button, gear_click, LV_EVENT_CLICKED, f);
    f->settings_label = lv_label_create(f->settings_button);
    lv_label_set_text(f->settings_label, LV_SYMBOL_SETTINGS);
    lv_obj_center(f->settings_label);
    show(f->settings_button, false);

    f->settings_panel = lv_obj_create(f->root);
    lv_obj_set_size(f->settings_panel, 220, 310);
    lv_obj_set_style_radius(f->settings_panel, 18, 0);
    lv_obj_set_style_border_width(f->settings_panel, 0, 0);
    lv_obj_set_style_pad_all(f->settings_panel, 10, 0);
    lv_obj_remove_flag(f->settings_panel, LV_OBJ_FLAG_SCROLLABLE);
    f->panel_title = lv_label_create(f->settings_panel);
    lv_label_set_text(f->panel_title, "Theme");
    lv_obj_align(f->panel_title, LV_ALIGN_TOP_MID, 0, 2);
    for (size_t i = 0; i < FACE_THEME_COUNT && i < 5; ++i) {
        lv_obj_t *button = lv_button_create(f->settings_panel);
        f->theme_buttons[i] = button;
        lv_obj_set_size(button, 186, 40);
        lv_obj_set_style_radius(button, 12, 0);
        lv_obj_set_style_border_width(button, 0, 0);
        lv_obj_align(button, LV_ALIGN_TOP_MID, 0, 31 + (int)i * 49);
        lv_obj_add_event_cb(button, theme_click, LV_EVENT_CLICKED, f);
        lv_obj_t *label = lv_label_create(button);
        lv_label_set_text(label, k_face_palettes[i].name);
        lv_obj_center(label);
    }
    show(f->settings_panel, false);
    lv_obj_update_layout(f->root);
    repaint(f);
    layout(f);
    f->tick = lv_timer_create(tick, 33, f);
    return f;
}

void face_destroy(face_view_t *f) {
    if (!f) return;
    if (f->tick) lv_timer_delete(f->tick);
    if (f->root) lv_obj_delete(f->root);
    free(f);
}

bool face_set_theme(face_view_t *f, const char *theme_id) {
    if (!f || !theme_id) return false;
    for (size_t i = 0; i < FACE_THEME_COUNT; ++i) {
        if (strcmp(theme_id, k_face_palettes[i].id) == 0) {
            f->palette = &k_face_palettes[i];
            repaint(f);
            return true;
        }
    }
    return false;
}

bool face_set_expression(face_view_t *f, const char *name) {
    if (!f || !name) return false;
    for (int i = 0; i < (int)(sizeof(EXPRESSIONS) / sizeof(EXPRESSIONS[0])); ++i) {
        if (strcmp(name, EXPRESSIONS[i]) == 0) {
            f->expression = i;
            f->blinking = false;
            f->last_blink = lv_tick_get();
            layout(f);
            return true;
        }
    }
    return false;
}

void face_set_gaze(face_view_t *f, int x, int y) {
    if (!f) return;
    f->target_x = clamp(x, -100, 100);
    f->target_y = clamp(y, -100, 100);
}

void face_set_subtitle(face_view_t *f, const char *text) {
    if (!f) return;
    if (!text || !*text) {
        lv_label_set_text(f->caption, "");
        show(f->caption, false);
        show(f->caption_bar, false);
        return;
    }
    char buf[384];
    size_t n = strlen(text);
    if (n >= sizeof(buf)) n = sizeof(buf) - 1;
    /* Never end in the middle of a UTF-8 continuation byte. */
    while (n > 0 && (((unsigned char)text[n]) & 0xC0u) == 0x80u) n--;
    memcpy(buf, text, n);
    buf[n] = '\0';
    lv_label_set_text(f->caption, buf);
    show(f->caption, true);
    show(f->caption_bar, true);
}

void face_reveal_settings(face_view_t *f) {
    if (!f) return;
    show(f->settings_button, true);
    f->settings_until = lv_tick_get() + 6500;
}

const char *face_theme_id(const face_view_t *f) {
    return f ? f->palette->id : NULL;
}
const char *face_expression_id(const face_view_t *f) {
    return f ? EXPRESSIONS[f->expression] : NULL;
}
lv_obj_t *face_root(face_view_t *f) { return f ? f->root : NULL; }
