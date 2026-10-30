/*
 * Face settings navigation: Face -> Settings -> Appearance -> Apply -> Face.
 *
 * UI owns no visual eye geometry. Theme cards reuse the renderer selected
 * by each appearance pack (outline, pill, or future external driver).
 * Selection is tentative until Apply; the Face controller owns applying it,
 * and the app owns persistence.
 */
#include "face_settings.h"
#include "face_renderer.h"
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

typedef enum { PAGE_CLOSED, PAGE_SETTINGS, PAGE_APPEARANCE } settings_page_t;

struct face_settings {
    lv_obj_t *root, *settings_page, *appearance_page;
    lv_obj_t *settings_title, *dismiss_button, *dismiss_label;
    lv_obj_t *appearance_row, *appearance_label, *appearance_description;
    lv_obj_t *active_swatch, *chevron;
    lv_obj_t *appearance_title, *back_button, *back_label, *help_label;
    lv_obj_t *theme_cards[FACE_THEME_COUNT];
    lv_obj_t *theme_previews[FACE_THEME_COUNT];
    face_renderer_t *theme_renderers[FACE_THEME_COUNT];
    lv_obj_t *theme_labels[FACE_THEME_COUNT], *selection_marks[FACE_THEME_COUNT];
    lv_obj_t *cancel_button, *cancel_label, *apply_button, *apply_label;
    face_settings_apply_cb_t on_apply;
    void *context;
    settings_page_t page;
    size_t active_index, pending_index;
};

static void show(lv_obj_t *o, bool visible) {
    if (visible) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}
static void put(lv_obj_t *o, int x, int y, int w, int h) {
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
}
static size_t find_index(const char *id) {
    if (id) {
        for (size_t i = 0; i < FACE_THEME_COUNT; i++)
            if (strcmp(k_face_themes[i].id, id) == 0) return i;
    }
    return 0;
}
static lv_obj_t *panel(lv_obj_t *parent, bool clickable) {
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_radius(o, 0, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
    if (clickable) lv_obj_add_flag(o, LV_OBJ_FLAG_CLICKABLE);
    else lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE);
    return o;
}
static lv_obj_t *button(lv_obj_t *parent) {
    lv_obj_t *o = lv_button_create(parent);
    lv_obj_set_style_radius(o, 14, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
    return o;
}
static lv_obj_t *label(lv_obj_t *parent, const char *zh, const char *en) {
    lv_obj_t *o = lv_label_create(parent);
#if LV_FONT_SOURCE_HAN_SANS_SC_16_CJK
    lv_label_set_text(o, zh);
    lv_obj_set_style_text_font(o, &lv_font_source_han_sans_sc_16_cjk, 0);
#else
    lv_label_set_text(o, en);
#endif
    return o;
}

static void paint(face_settings_t *s) {
    const face_theme_t *active = &k_face_themes[s->active_index];
    lv_color_t bg = lv_color_hex(active->background);
    lv_color_t tile = lv_color_hex(active->subtitle_bg);
    lv_color_t fg = lv_color_hex(active->subtitle_text);
    lv_color_t highlight = lv_color_hex(active->eye);
    lv_obj_set_style_bg_color(s->root, bg, 0);
    lv_obj_set_style_bg_color(s->settings_page, bg, 0);
    lv_obj_set_style_bg_color(s->appearance_page, bg, 0);
    lv_obj_set_style_bg_color(s->appearance_row, tile, 0);
    lv_obj_set_style_bg_color(s->dismiss_button, tile, 0);
    lv_obj_set_style_bg_color(s->back_button, tile, 0);
    lv_obj_set_style_bg_color(s->cancel_button, tile, 0);
    lv_obj_set_style_bg_color(s->active_swatch, highlight, 0);
    lv_obj_set_style_bg_color(s->apply_button, highlight, 0);

    lv_obj_t *texts[] = {
        s->settings_title, s->appearance_label, s->appearance_description,
        s->chevron, s->dismiss_label, s->appearance_title, s->help_label,
        s->back_label, s->cancel_label,
    };
    for (size_t i = 0; i < sizeof(texts) / sizeof(texts[0]); i++)
        lv_obj_set_style_text_color(texts[i], fg, 0);
    lv_obj_set_style_text_color(s->apply_label, bg, 0);

    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        const bool selected = i == s->pending_index;
        const face_theme_t *theme = &k_face_themes[i];
        lv_obj_set_style_bg_color(s->theme_cards[i], tile, 0);
        lv_obj_set_style_border_width(s->theme_cards[i], selected ? 3 : 1, 0);
        lv_obj_set_style_border_color(s->theme_cards[i],
                                      selected ? lv_color_hex(theme->eye) : tile, 0);
        lv_obj_set_style_bg_color(s->theme_previews[i],
                                  lv_color_hex(theme->background), 0);
        lv_obj_set_style_text_color(s->theme_labels[i], fg, 0);
        lv_obj_set_style_bg_color(s->selection_marks[i],
                                  lv_color_hex(theme->eye), 0);
        show(s->selection_marks[i], selected);
    }
}

static void settings_home(face_settings_t *s) {
    s->page = PAGE_SETTINGS;
    show(s->root, true);
    show(s->settings_page, true);
    show(s->appearance_page, false);
    paint(s);
}

void face_settings_close(face_settings_t *s) {
    if (!s) return;
    s->page = PAGE_CLOSED;
    s->pending_index = s->active_index;
    show(s->root, false);
}

static void close_clicked(lv_event_t *e) {
    face_settings_close(lv_event_get_user_data(e));
}

static void appearance_clicked(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    s->page = PAGE_APPEARANCE;
    s->pending_index = s->active_index;
    show(s->settings_page, false);
    show(s->appearance_page, true);
    paint(s);
}

static void back_clicked(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    s->pending_index = s->active_index; /* no uncommitted change */
    settings_home(s);
}

static void theme_clicked(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    lv_obj_t *pressed = lv_event_get_target_obj(e);
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        if (pressed == s->theme_cards[i]) {
            s->pending_index = i; /* select, but DO NOT apply */
            paint(s);
            return;
        }
    }
}

static void apply_clicked(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    if (s->page != PAGE_APPEARANCE) return;
    const size_t selected = s->pending_index;
    s->active_index = selected;
    /* Return directly to the animated face, not back through settings. */
    face_settings_close(s);
    if (s->on_apply) s->on_apply(k_face_themes[selected].id, s->context);
}

face_settings_t *face_settings_create(lv_obj_t *parent,
                                      face_settings_apply_cb_t on_apply,
                                      void *context) {
    if (!parent) return NULL;
    face_settings_t *s = calloc(1, sizeof(*s));
    if (!s) return NULL;
    s->on_apply = on_apply;
    s->context = context;
    s->page = PAGE_CLOSED;
    s->root = panel(parent, true);
    lv_obj_set_style_bg_opa(s->root, LV_OPA_COVER, 0);

    s->settings_page = panel(s->root, true);
    s->appearance_page = panel(s->root, true);
    s->settings_title = label(s->settings_page, "设置", "Settings");
    s->dismiss_button = button(s->settings_page);
    s->dismiss_label = label(s->dismiss_button, "×", "X");
    lv_obj_center(s->dismiss_label);
    lv_obj_add_event_cb(s->dismiss_button, close_clicked, LV_EVENT_CLICKED, s);
    s->appearance_row = button(s->settings_page);
    lv_obj_add_event_cb(s->appearance_row, appearance_clicked, LV_EVENT_CLICKED, s);
    s->appearance_label = label(s->appearance_row, "外观", "Appearance");
    s->appearance_description = label(s->appearance_row, "配色与眼睛风格", "Themes and eye styles");
    s->active_swatch = panel(s->appearance_row, false);
    lv_obj_set_style_radius(s->active_swatch, LV_RADIUS_CIRCLE, 0);
    s->chevron = label(s->appearance_row, ">", ">");

    s->appearance_title = label(s->appearance_page, "外观", "Appearance");
    s->back_button = button(s->appearance_page);
    s->back_label = label(s->back_button, "<", "<");
    lv_obj_center(s->back_label);
    lv_obj_add_event_cb(s->back_button, back_clicked, LV_EVENT_CLICKED, s);
    s->help_label = label(s->appearance_page, "选择一个外观，点击应用后生效",
                          "Choose an appearance; tap Apply to confirm");
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        s->theme_cards[i] = button(s->appearance_page);
        lv_obj_add_event_cb(s->theme_cards[i], theme_clicked, LV_EVENT_CLICKED, s);
        s->theme_previews[i] = panel(s->theme_cards[i], false);
        lv_obj_set_style_radius(s->theme_previews[i], 12, 0);
        s->theme_renderers[i] = face_renderer_create(k_face_themes[i].renderer_id,
                                                     s->theme_previews[i]);
        if (!s->theme_renderers[i]) {
            face_settings_destroy(s);
            return NULL;
        }
        s->theme_labels[i] = label(s->theme_cards[i],
                                    k_face_themes[i].name_zh, k_face_themes[i].name);
        s->selection_marks[i] = panel(s->theme_cards[i], false);
        lv_obj_set_style_radius(s->selection_marks[i], LV_RADIUS_CIRCLE, 0);
    }
    s->cancel_button = button(s->appearance_page);
    s->cancel_label = label(s->cancel_button, "返回", "Back");
    lv_obj_center(s->cancel_label);
    lv_obj_add_event_cb(s->cancel_button, back_clicked, LV_EVENT_CLICKED, s);
    s->apply_button = button(s->appearance_page);
    s->apply_label = label(s->apply_button, "应用", "Apply");
    lv_obj_center(s->apply_label);
    lv_obj_add_event_cb(s->apply_button, apply_clicked, LV_EVENT_CLICKED, s);

    face_settings_layout(s, 800, 480);
    paint(s);
    face_settings_close(s);
    return s;
}

void face_settings_destroy(face_settings_t *s) {
    if (!s) return;
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        if (s->theme_renderers[i]) face_renderer_destroy(s->theme_renderers[i]);
    }
    if (s->root) lv_obj_delete(s->root);
    free(s);
}

void face_settings_open(face_settings_t *s, const char *active_theme_id) {
    if (!s) return;
    s->active_index = find_index(active_theme_id);
    s->pending_index = s->active_index;
    lv_obj_move_foreground(s->root);
    settings_home(s);
}

bool face_settings_is_open(const face_settings_t *s) {
    return s && s->page != PAGE_CLOSED;
}

void face_settings_set_active_theme(face_settings_t *s, const char *id) {
    if (!s) return;
    s->active_index = find_index(id);
    if (s->page != PAGE_APPEARANCE) s->pending_index = s->active_index;
    paint(s);
}

void face_settings_layout(face_settings_t *s, int w, int h) {
    if (!s || w < 350 || h < 240) return;
    put(s->root, 0, 0, w, h);
    put(s->settings_page, 0, 0, w, h);
    put(s->appearance_page, 0, 0, w, h);
    put(s->settings_title, 62, 44, 320, 34);
    put(s->dismiss_button, w - 98, 28, 56, 48);
    put(s->appearance_row, w * 8 / 100, h * 32 / 100, w * 84 / 100, 116);
    put(s->appearance_label, 30, 24, 300, 27);
    put(s->appearance_description, 30, 66, 340, 27);
    put(s->active_swatch, w * 66 / 100, 35, 40, 40);
    put(s->chevron, w * 73 / 100, 38, 40, 40);
    put(s->back_button, 37, 28, 52, 47);
    put(s->appearance_title, 110, 43, 320, 30);
    put(s->help_label, 38, 93, w - 76, 26);
    const int margin = w * 4 / 100;
    const int gap = w * 15 / 1000;
    const int card_w = (w - 2 * margin - (int)(FACE_THEME_COUNT - 1) * gap) /
                       (int)FACE_THEME_COUNT;
    const int card_h = h * 41 / 100;
    const int card_top = h * 28 / 100;
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        int x = margin + (int)i * (card_w + gap);
        put(s->theme_cards[i], x, card_top, card_w, card_h);
        const int pw = card_w - 16, ph = card_h * 55 / 100;
        put(s->theme_previews[i], 8, 10, pw, ph);
        const face_render_state_t idle = {.expression=FACE_EX_IDLE,
                                           .gaze_x=0, .gaze_y=0, .blinking=false};
        face_renderer_render(s->theme_renderers[i], &k_face_themes[i],
                             &idle, pw, ph);
        put(s->theme_labels[i], 8, card_h * 71 / 100, card_w - 16, 26);
        lv_obj_set_style_text_align(s->theme_labels[i], LV_TEXT_ALIGN_CENTER, 0);
        put(s->selection_marks[i], card_w - 21, 8, 10, 10);
    }
    put(s->cancel_button, w / 2 - 190, h - 83, 136, 55);
    put(s->apply_button, w / 2 - 27, h - 83, 205, 55);
}
