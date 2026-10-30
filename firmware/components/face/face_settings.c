/*
 * Face settings: main settings screen -> Appearance submenu -> Apply/Cancel.
 * Only a selected and explicitly Applied palette changes the live face.
 * The component does not know about NVS, networking, audio or hardware GPIO.
 */
#include "face_settings.h"
#include "face_themes_generated.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef enum { PAGE_CLOSED, PAGE_HOME, PAGE_APPEARANCE } page_t;

struct face_settings {
    lv_obj_t *root;
    lv_obj_t *home;
    lv_obj_t *appearance;
    lv_obj_t *home_header;
    lv_obj_t *close_button, *close_label;
    lv_obj_t *appearance_row, *appearance_title, *appearance_description;
    lv_obj_t *home_swatch, *home_arrow;
    lv_obj_t *appearance_header, *back_button, *back_label;
    lv_obj_t *appearance_description_label;
    lv_obj_t *cards[FACE_THEME_COUNT];
    lv_obj_t *card_preview[FACE_THEME_COUNT];
    lv_obj_t *card_outer[FACE_THEME_COUNT][2];
    lv_obj_t *card_inner[FACE_THEME_COUNT][2];
    lv_obj_t *card_dot[FACE_THEME_COUNT][2];
    lv_obj_t *card_label[FACE_THEME_COUNT];
    lv_obj_t *card_marker[FACE_THEME_COUNT];
    lv_obj_t *apply_button, *apply_label, *cancel_button, *cancel_label;
    face_settings_apply_cb_t on_apply;
    void *context;
    page_t page;
    size_t active_index, pending_index;
};

static void visible(lv_obj_t *o, bool yes) {
    if (yes) lv_obj_remove_flag(o, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_add_flag(o, LV_OBJ_FLAG_HIDDEN);
}

static void place(lv_obj_t *o, int x, int y, int w, int h) {
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
}

/* Neutral base object without unwanted scroll bars/shadows. */
static lv_obj_t *plain(lv_obj_t *parent, bool clickable) {
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_set_style_radius(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
    if (clickable) lv_obj_add_flag(o, LV_OBJ_FLAG_CLICKABLE);
    else lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE);
    return o;
}

static lv_obj_t *pill(lv_obj_t *parent) {
    lv_obj_t *o = plain(parent, false);
    lv_obj_set_style_radius(o, LV_RADIUS_CIRCLE, 0);
    return o;
}

static void set_font(lv_obj_t *label) {
#if LV_FONT_SOURCE_HAN_SANS_SC_16_CJK
    lv_obj_set_style_text_font(label, &lv_font_source_han_sans_sc_16_cjk, 0);
#else
    (void)label;
#endif
}
static lv_obj_t *caption(lv_obj_t *parent, const char *zh, const char *en) {
    lv_obj_t *o = lv_label_create(parent);
#if LV_FONT_SOURCE_HAN_SANS_SC_16_CJK
    lv_label_set_text(o, zh);
#else
    lv_label_set_text(o, en);
#endif
    set_font(o);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE);
    return o;
}
static lv_obj_t *button(lv_obj_t *parent) {
    lv_obj_t *o = lv_button_create(parent);
    lv_obj_set_style_radius(o, 15, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
    return o;
}

static size_t index_for(const char *id) {
    if (id) {
        for (size_t i = 0; i < FACE_THEME_COUNT; ++i) {
            if (strcmp(id, k_face_palettes[i].id) == 0) return i;
        }
    }
    return 0;
}

static void paint(face_settings_t *s) {
    const face_theme_palette_t *a = &k_face_palettes[s->active_index];
    const lv_color_t bg = lv_color_hex(a->background);
    const lv_color_t tile = lv_color_hex(a->subtitle_bg);
    const lv_color_t text = lv_color_hex(a->subtitle_text);
    const lv_color_t accent = lv_color_hex(a->eye);
    lv_obj_set_style_bg_color(s->root, bg, 0);
    lv_obj_set_style_bg_color(s->home, bg, 0);
    lv_obj_set_style_bg_color(s->appearance, bg, 0);

    lv_obj_t *labels[] = {
        s->home_header, s->appearance_title, s->appearance_description,
        s->home_arrow, s->appearance_header, s->back_label,
        s->close_label, s->appearance_description_label, s->cancel_label
    };
    for (size_t i = 0; i < sizeof(labels) / sizeof(labels[0]); i++) {
        lv_obj_set_style_text_color(labels[i], text, 0);
    }
    lv_obj_set_style_bg_color(s->appearance_row, tile, 0);
    lv_obj_set_style_text_color(s->home_swatch, accent, 0);
    lv_obj_set_style_bg_color(s->home_swatch, accent, 0);
    lv_obj_set_style_bg_color(s->close_button, tile, 0);
    lv_obj_set_style_bg_color(s->back_button, tile, 0);
    lv_obj_set_style_bg_color(s->cancel_button, tile, 0);
    lv_obj_set_style_bg_color(s->apply_button, accent, 0);
    lv_obj_set_style_text_color(s->apply_label, bg, 0);

    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        const face_theme_palette_t *p = &k_face_palettes[i];
        const bool selected = s->pending_index == i;
        lv_obj_set_style_bg_color(s->cards[i], tile, 0);
        lv_obj_set_style_border_width(s->cards[i], selected ? 3 : 1, 0);
        lv_obj_set_style_border_color(s->cards[i],
                                      selected ? lv_color_hex(p->eye) : tile, 0);
        lv_obj_set_style_bg_color(s->card_preview[i], lv_color_hex(p->background), 0);
        lv_obj_set_style_text_color(s->card_label[i], text, 0);
        lv_obj_set_style_bg_color(s->card_marker[i], lv_color_hex(p->eye), 0);
        visible(s->card_marker[i], selected);
        for (int j = 0; j < 2; j++) {
            lv_obj_set_style_bg_color(s->card_outer[i][j],
                                      lv_color_hex(p->eye), 0);
            lv_obj_set_style_bg_color(s->card_inner[i][j],
                                      lv_color_hex(p->background), 0);
            lv_obj_set_style_bg_color(s->card_dot[i][j],
                                      lv_color_hex(p->eye), 0);
        }
    }
}

static void show_home(face_settings_t *s) {
    s->page = PAGE_HOME;
    visible(s->root, true);
    visible(s->home, true);
    visible(s->appearance, false);
    paint(s);
}

static void go_home(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    s->pending_index = s->active_index; /* discard un-applied selection */
    show_home(s);
}

void face_settings_close(face_settings_t *s) {
    if (!s) return;
    s->pending_index = s->active_index;
    s->page = PAGE_CLOSED;
    visible(s->root, false);
}

static void dismiss(lv_event_t *e) {
    face_settings_close(lv_event_get_user_data(e));
}

static void open_appearance(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    s->pending_index = s->active_index;
    s->page = PAGE_APPEARANCE;
    visible(s->home, false);
    visible(s->appearance, true);
    paint(s);
}

static void choose_card(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    lv_obj_t *pressed = lv_event_get_target_obj(e);
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        if (pressed == s->cards[i]) {
            s->pending_index = i;
            paint(s); /* selection only: does not recolor face */
            break;
        }
    }
}

static void apply(lv_event_t *e) {
    face_settings_t *s = lv_event_get_user_data(e);
    if (s->page != PAGE_APPEARANCE) return;
    size_t index = s->pending_index;
    s->active_index = index;
    face_settings_close(s); /* return to face even when callback updates palette */
    if (s->on_apply) {
        s->on_apply(k_face_palettes[index].id, s->context);
    }
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
    s->root = plain(parent, true);
    lv_obj_set_style_bg_opa(s->root, LV_OPA_COVER, 0);

    s->home = plain(s->root, true);
    s->appearance = plain(s->root, true);

    s->home_header = caption(s->home, "设置", "Settings");
    s->close_button = button(s->home);
    s->close_label = caption(s->close_button, "×", "X");
    lv_obj_center(s->close_label);
    lv_obj_add_event_cb(s->close_button, dismiss, LV_EVENT_CLICKED, s);
    s->appearance_row = button(s->home);
    lv_obj_add_event_cb(s->appearance_row, open_appearance, LV_EVENT_CLICKED, s);
    s->appearance_title = caption(s->appearance_row, "外观", "Appearance");
    s->appearance_description = caption(s->appearance_row, "选择配色", "Color themes");
    s->home_swatch = pill(s->appearance_row);
    s->home_arrow = caption(s->appearance_row, ">", ">");

    s->appearance_header = caption(s->appearance, "外观", "Appearance");
    s->back_button = button(s->appearance);
    s->back_label = caption(s->back_button, "<", "<");
    lv_obj_center(s->back_label);
    lv_obj_add_event_cb(s->back_button, go_home, LV_EVENT_CLICKED, s);
    s->appearance_description_label = caption(s->appearance, "选择配色", "Choose a theme");

    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        s->cards[i] = button(s->appearance);
        lv_obj_add_event_cb(s->cards[i], choose_card, LV_EVENT_CLICKED, s);
        s->card_preview[i] = plain(s->cards[i], false);
        lv_obj_set_style_radius(s->card_preview[i], 12, 0);
        for (int j = 0; j < 2; j++) {
            s->card_outer[i][j] = pill(s->card_preview[i]);
            s->card_inner[i][j] = pill(s->card_preview[i]);
            s->card_dot[i][j] = pill(s->card_preview[i]);
        }
        s->card_label[i] = caption(s->cards[i], k_face_palettes[i].name_zh,
                                   k_face_palettes[i].name);
        s->card_marker[i] = pill(s->cards[i]);
    }
    s->cancel_button = button(s->appearance);
    s->cancel_label = caption(s->cancel_button, "返回", "Back");
    lv_obj_center(s->cancel_label);
    lv_obj_add_event_cb(s->cancel_button, go_home, LV_EVENT_CLICKED, s);

    s->apply_button = button(s->appearance);
    s->apply_label = caption(s->apply_button, "应用", "Apply");
    lv_obj_center(s->apply_label);
    lv_obj_add_event_cb(s->apply_button, apply, LV_EVENT_CLICKED, s);

    /* Default 800x480; call again when parent is laid out. */
    face_settings_layout(s, 800, 480);
    paint(s);
    face_settings_close(s);
    return s;
}

void face_settings_destroy(face_settings_t *s) {
    if (!s) return;
    lv_obj_delete(s->root);
    free(s);
}

void face_settings_open(face_settings_t *s, const char *active_theme_id) {
    if (!s) return;
    s->active_index = index_for(active_theme_id);
    s->pending_index = s->active_index;
    lv_obj_move_foreground(s->root);
    show_home(s);
}

bool face_settings_is_open(const face_settings_t *s) {
    return s && s->page != PAGE_CLOSED;
}

void face_settings_set_active_theme(face_settings_t *s, const char *theme_id) {
    if (!s) return;
    s->active_index = index_for(theme_id);
    /* Do not cancel the selection currently being inspected. */
    if (s->page != PAGE_APPEARANCE) s->pending_index = s->active_index;
    paint(s);
}

void face_settings_layout(face_settings_t *s, int w, int h) {
    if (!s || w < 350 || h < 240) return;
    place(s->root, 0, 0, w, h);
    place(s->home, 0, 0, w, h);
    place(s->appearance, 0, 0, w, h);

    place(s->home_header, 62, 43, 380, 32);
    place(s->close_button, w - 92, 28, 56, 48);
    place(s->appearance_row, 64, h * 33 / 100, w - 128, 116);
    place(s->appearance_title, 33, 24, 260, 26);
    place(s->appearance_description, 33, 66, 310, 26);
    place(s->home_swatch, w - 226, 38, 39, 39);
    place(s->home_arrow, w - 175, 38, 45, 42);

    place(s->back_button, 36, 28, 54, 47);
    place(s->appearance_header, 109, 43, 240, 32);
    place(s->appearance_description_label, 38, 93, 340, 26);
    const int gap = 12, margin = 34;
    const int card_w = (w - 2 * margin - 4 * gap) / 5;
    const int top = h * 27 / 100, card_h = h * 42 / 100;
    for (size_t i = 0; i < FACE_THEME_COUNT; i++) {
        int x = margin + (int)i * (card_w + gap);
        place(s->cards[i], x, top, card_w, card_h);
        place(s->card_preview[i], 8, 11, card_w - 16, card_h * 54 / 100);
        int preview_w = card_w - 16, preview_h = card_h * 54 / 100;
        const int ew = preview_w * 25 / 100, eh = preview_h * 44 / 100;
        for (int j = 0; j < 2; j++) {
            const int cx = preview_w * (j == 0 ? 30 : 70) / 100;
            const int cy = preview_h * 50 / 100;
            const int inset = 6;
            place(s->card_outer[i][j], cx - ew/2, cy - eh/2, ew, eh);
            place(s->card_inner[i][j], cx - ew/2 + inset,
                  cy - eh/2 + inset, ew - 2*inset, eh - 2*inset);
            place(s->card_dot[i][j], cx + 1, cy - 9, 8, 8);
        }
        place(s->card_label[i], 10, card_h * 70 / 100, card_w - 20, 29);
        lv_obj_set_style_text_align(s->card_label[i], LV_TEXT_ALIGN_CENTER, 0);
        place(s->card_marker[i], card_w - 22, 8, 10, 10);
    }
    place(s->cancel_button, w / 2 - 190, h - 83, 133, 56);
    place(s->apply_button, w / 2 - 29, h - 83, 205, 56);
}
