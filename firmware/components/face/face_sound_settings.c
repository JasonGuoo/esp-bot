/* LVGL 9 sound preferences and metadata-only diagnostics.
 * Slider/switch event handling follows the official v9.6 examples linked in
 * README.md. All callbacks only request application work; no codec or runtime
 * dependency, persistence, playback, recording or conversation decision here. */
#include "face_sound_settings.h"
#include "fonts/xiaodai_font.h"
#include <stdlib.h>
#include <string.h>

enum { ROW_VOLUME, ROW_MUTE, ROW_MODE, ROW_WAKE, ROW_NIGHT, ROW_DIAGNOSTICS, ROW_COUNT };
enum { TEXT_TITLE, TEXT_BACK, TEXT_VOLUME, TEXT_VOLUME_VALUE, TEXT_MUTE, TEXT_MUTE_HELP,
       TEXT_MODE, TEXT_MODE_VALUE, TEXT_MODE_HELP, TEXT_WAKE, TEXT_WAKE_HELP,
       TEXT_NIGHT, TEXT_NIGHT_VALUE, TEXT_NIGHT_HELP, TEXT_PREVIEW, TEXT_DEFAULT,
       TEXT_PREVIEW_HELP, TEXT_NOTICE, TEXT_DIAGNOSTIC_TITLE, TEXT_DIAGNOSTIC, TEXT_COUNT };
static const uint8_t k_wake_thresholds[] = {90, 80, 70};

struct face_sound_settings {
    lv_obj_t *root, *body, *back, *rows[ROW_COUNT], *texts[TEXT_COUNT];
    lv_obj_t *volume, *mute, *mode, *night, *preview, *defaults;
    lv_obj_t *wake_choices[3], *wake_labels[3];
    bot_voice_settings_t value, authoritative;
    bot_voice_diagnostics_t diagnostics;
    bool authoritative_valid, diagnostics_valid, syncing;
    bool (*on_request)(const bot_voice_settings_t *, void *);
    bool (*on_preview)(void *);
    void (*on_back)(void *);
    void *request_context, *preview_context, *back_context;
    lv_color_t background, tile, text, accent;
    char notice[192];
};

static void put(lv_obj_t *o, int x, int y, int width, int height) {
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, width, height);
}

static lv_obj_t *panel(lv_obj_t *parent) {
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_radius(o, 14, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
    return o;
}

static lv_obj_t *button(lv_obj_t *parent) {
    lv_obj_t *o = lv_button_create(parent);
    lv_obj_set_style_pad_all(o, 0, 0);
    lv_obj_set_style_radius(o, 12, 0);
    lv_obj_set_style_border_width(o, 0, 0);
    lv_obj_set_style_shadow_width(o, 0, 0);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
    return o;
}

static lv_obj_t *label(lv_obj_t *parent, const char *text) {
    lv_obj_t *o = lv_label_create(parent);
    lv_label_set_text(o, text);
    lv_obj_set_style_text_font(o, &xiaodai_font_sc_22, 0);
    return o;
}

static void checked(lv_obj_t *o, bool check) {
    if (check) lv_obj_add_state(o, LV_STATE_CHECKED);
    else lv_obj_remove_state(o, LV_STATE_CHECKED);
}

static void disabled(lv_obj_t *o, bool disable) {
    if (disable) lv_obj_add_state(o, LV_STATE_DISABLED);
    else lv_obj_remove_state(o, LV_STATE_DISABLED);
}

static void paint(face_sound_settings_t *s) {
    lv_obj_set_style_bg_color(s->root, s->background, 0);
    lv_obj_set_style_bg_color(s->back, s->tile, 0);
    for (size_t i = 0; i < ROW_COUNT; ++i)
        lv_obj_set_style_bg_color(s->rows[i], s->tile, 0);
    for (size_t i = 0; i < TEXT_COUNT; ++i)
        lv_obj_set_style_text_color(s->texts[i], s->text, 0);
    lv_obj_t *sliders[] = {s->volume, s->night};
    for (size_t i = 0; i < 2; ++i) {
        lv_obj_set_style_bg_color(sliders[i], s->text, LV_PART_MAIN);
        lv_obj_set_style_bg_opa(sliders[i], LV_OPA_30, LV_PART_MAIN);
        lv_obj_set_style_bg_color(sliders[i], s->accent, LV_PART_INDICATOR);
        lv_obj_set_style_bg_color(sliders[i], s->accent, LV_PART_KNOB);
        lv_obj_set_style_pad_all(sliders[i], 10, LV_PART_KNOB);
    }
    lv_obj_t *switches[] = {s->mute, s->mode};
    for (size_t i = 0; i < 2; ++i) {
        lv_obj_set_style_bg_color(switches[i], s->text, LV_PART_MAIN);
        lv_obj_set_style_bg_opa(switches[i], LV_OPA_30, LV_PART_MAIN);
        lv_obj_set_style_bg_color(switches[i], s->accent,
                                   LV_PART_INDICATOR | LV_STATE_CHECKED);
        lv_obj_set_style_bg_color(switches[i], s->text, LV_PART_KNOB);
    }
    for (size_t i = 0; i < 3; ++i) {
        bool selected = s->value.wake_threshold_percent == k_wake_thresholds[i];
        lv_obj_set_style_bg_color(s->wake_choices[i], selected ? s->accent : s->background, 0);
        lv_obj_set_style_text_color(s->wake_labels[i], selected ? s->background : s->text, 0);
    }
    lv_obj_set_style_bg_color(s->preview, s->accent, 0);
    lv_obj_set_style_text_color(s->texts[TEXT_PREVIEW], s->background, 0);
    lv_obj_set_style_bg_color(s->defaults, s->tile, 0);
}

static void refresh_controls(face_sound_settings_t *s) {
    s->syncing = true;
    /* A background status refresh must not move a knob under the user's finger. */
    if (!lv_slider_is_dragged(s->volume))
        lv_slider_set_value(s->volume, s->value.volume_percent, LV_ANIM_OFF);
    if (!lv_slider_is_dragged(s->night))
        lv_slider_set_value(s->night, s->value.night_gain_percent, LV_ANIM_OFF);
    lv_label_set_text_fmt(s->texts[TEXT_VOLUME_VALUE], "%ld%%", (long)lv_slider_get_value(s->volume));
    lv_label_set_text_fmt(s->texts[TEXT_NIGHT_VALUE], "%ld%%", (long)lv_slider_get_value(s->night));
    checked(s->mute, s->value.muted);
    checked(s->mode, s->value.spoken_answers);
    lv_label_set_text(s->texts[TEXT_MODE_VALUE], s->value.spoken_answers ? "图文 + 语音" : "仅图文");
    const char *wake_help = "只识别完整的“小呆小呆”";
    if (s->value.wake_threshold_percent == k_wake_thresholds[0])
        wake_help = "低档更严格，可能需要叫得更清楚";
    else if (s->value.wake_threshold_percent == k_wake_thresholds[2])
        wake_help = "高档更容易唤醒，也更容易误触发";
    else if (s->value.wake_threshold_percent != k_wake_thresholds[1])
        wake_help = "当前为自定义档位，选择后才会更改";
    lv_label_set_text(s->texts[TEXT_WAKE_HELP], wake_help);
    const bool output_muted = s->value.muted || s->value.volume_percent == 0;
    lv_label_set_text(s->texts[TEXT_PREVIEW_HELP], output_muted ?
        "请先取消静音并调高音量，再试听" : "试听本地语音，不开启对话");
    disabled(s->preview, output_muted || !s->on_preview);
    lv_obj_t *controls[] = {s->volume, s->mute, s->mode, s->night, s->defaults,
                            s->wake_choices[0], s->wake_choices[1], s->wake_choices[2]};
    for (size_t i = 0; i < sizeof(controls) / sizeof(controls[0]); ++i)
        disabled(controls[i], !s->on_request);
    s->syncing = false;
    paint(s);
}

void face_sound_settings_set_notice(face_sound_settings_t *s, const char *notice) {
    if (!s) return;
    if (!notice) notice = "";
    size_t length = strlen(notice);
    if (length >= sizeof(s->notice)) {
        length = sizeof(s->notice) - 1;
        while (length && ((unsigned char)notice[length] & 0xc0U) == 0x80U) --length;
    }
    if (strlen(s->notice) == length && memcmp(s->notice, notice, length) == 0) return;
    memcpy(s->notice, notice, length);
    s->notice[length] = '\0';
    lv_label_set_text_static(s->texts[TEXT_NOTICE], s->notice);
}

static void request(face_sound_settings_t *s, const bot_voice_settings_t *candidate) {
    if (s->syncing || !bot_voice_settings_valid(candidate)) return;
    if (bot_voice_settings_equal(&s->value, candidate)) return;
    if (!s->on_request || !s->on_request(candidate, s->request_context)) {
        face_sound_settings_set_notice(s, "暂时无法修改，请重试");
        refresh_controls(s);
        return;
    }
    s->value = *candidate;
    face_sound_settings_set_notice(s, "正在应用声音设置");
    refresh_controls(s);
}

static void slider_event(lv_event_t *e) {
    face_sound_settings_t *s = lv_event_get_user_data(e);
    if (s->syncing) return;
    lv_obj_t *target = lv_event_get_target_obj(e);
    int32_t value = lv_slider_get_value(target);
    const bool volume = target == s->volume;
    lv_label_set_text_fmt(s->texts[volume ? TEXT_VOLUME_VALUE : TEXT_NIGHT_VALUE], "%ld%%", (long)value);
    /* Official slider VALUE_CHANGED is continuous. Commit only the final value. */
    if (lv_event_get_code(e) != LV_EVENT_RELEASED) return;
    bot_voice_settings_t candidate = s->value;
    if (volume) candidate.volume_percent = (uint8_t)value;
    else candidate.night_gain_percent = (uint8_t)value;
    request(s, &candidate);
}

static void switch_changed(lv_event_t *e) {
    face_sound_settings_t *s = lv_event_get_user_data(e);
    if (s->syncing) return;
    lv_obj_t *target = lv_event_get_target_obj(e);
    bot_voice_settings_t candidate = s->value;
    if (target == s->mute) candidate.muted = lv_obj_has_state(target, LV_STATE_CHECKED);
    else candidate.spoken_answers = lv_obj_has_state(target, LV_STATE_CHECKED);
    request(s, &candidate);
}

static void wake_clicked(lv_event_t *e) {
    face_sound_settings_t *s = lv_event_get_user_data(e);
    lv_obj_t *target = lv_event_get_target_obj(e);
    for (size_t i = 0; i < 3; ++i) {
        if (target != s->wake_choices[i]) continue;
        bot_voice_settings_t candidate = s->value;
        candidate.wake_threshold_percent = k_wake_thresholds[i];
        request(s, &candidate);
        break;
    }
}

static void preview_clicked(lv_event_t *e) {
    face_sound_settings_t *s = lv_event_get_user_data(e);
    if (s->value.muted || s->value.volume_percent == 0) return;
    bool accepted = s->on_preview && s->on_preview(s->preview_context);
    face_sound_settings_set_notice(s, accepted ? "已请求试听" : "暂时无法试听，请重试");
}

static void defaults_clicked(lv_event_t *e) {
    face_sound_settings_t *s = lv_event_get_user_data(e);
    const bot_voice_settings_t defaults = bot_voice_default_settings();
    if (bot_voice_settings_equal(&s->value, &defaults)) {
        face_sound_settings_set_notice(s, "当前已是默认声音设置");
        return;
    }
    request(s, &defaults);
}

static void back_clicked(lv_event_t *e) {
    face_sound_settings_t *s = lv_event_get_user_data(e);
    if (s->on_back) s->on_back(s->back_context);
}

face_sound_settings_t *face_sound_settings_create(lv_obj_t *parent,
                                                 void (*on_back)(void *), void *context) {
    if (!parent) return NULL;
    face_sound_settings_t *s = calloc(1, sizeof(*s));
    if (!s) return NULL;
    s->value = bot_voice_default_settings();
    s->on_back = on_back;
    s->back_context = context;
    s->root = panel(parent);
    lv_obj_set_style_radius(s->root, 0, 0);
    s->back = button(s->root);
    s->texts[TEXT_BACK] = label(s->back, "<");
    lv_obj_center(s->texts[TEXT_BACK]);
    lv_obj_add_event_cb(s->back, back_clicked, LV_EVENT_CLICKED, s);
    s->texts[TEXT_TITLE] = label(s->root, "声音");
    s->body = panel(s->root);
    lv_obj_set_style_bg_opa(s->body, LV_OPA_TRANSP, 0);
    lv_obj_add_flag(s->body, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_scroll_dir(s->body, LV_DIR_VER);
    lv_obj_set_scrollbar_mode(s->body, LV_SCROLLBAR_MODE_AUTO);
    for (size_t i = 0; i < ROW_COUNT; ++i) s->rows[i] = panel(s->body);

    s->texts[TEXT_VOLUME] = label(s->rows[ROW_VOLUME], "音量");
    s->texts[TEXT_VOLUME_VALUE] = label(s->rows[ROW_VOLUME], "");
    s->volume = lv_slider_create(s->rows[ROW_VOLUME]);
    lv_slider_set_range(s->volume, 0, BOT_VOICE_VOLUME_MAX);
    lv_obj_set_ext_click_area(s->volume, 18);
    lv_obj_add_event_cb(s->volume, slider_event, LV_EVENT_VALUE_CHANGED, s);
    lv_obj_add_event_cb(s->volume, slider_event, LV_EVENT_RELEASED, s);

    s->texts[TEXT_MUTE] = label(s->rows[ROW_MUTE], "静音");
    s->texts[TEXT_MUTE_HELP] = label(s->rows[ROW_MUTE], "保留当前音量");
    s->mute = lv_switch_create(s->rows[ROW_MUTE]);
    lv_obj_add_event_cb(s->mute, switch_changed, LV_EVENT_VALUE_CHANGED, s);

    s->texts[TEXT_MODE] = label(s->rows[ROW_MODE], "回答方式");
    s->texts[TEXT_MODE_VALUE] = label(s->rows[ROW_MODE], "");
    s->texts[TEXT_MODE_HELP] = label(s->rows[ROW_MODE], "控制 Mac 回复；本地唤醒回应保留");
    s->mode = lv_switch_create(s->rows[ROW_MODE]);
    lv_obj_add_event_cb(s->mode, switch_changed, LV_EVENT_VALUE_CHANGED, s);

    s->texts[TEXT_WAKE] = label(s->rows[ROW_WAKE], "唤醒灵敏度");
    s->texts[TEXT_WAKE_HELP] = label(s->rows[ROW_WAKE], "");
    const char *wake_names[] = {"低", "标准", "高"};
    for (size_t i = 0; i < 3; ++i) {
        s->wake_choices[i] = button(s->rows[ROW_WAKE]);
        s->wake_labels[i] = label(s->wake_choices[i], wake_names[i]);
        lv_obj_center(s->wake_labels[i]);
        lv_obj_add_event_cb(s->wake_choices[i], wake_clicked, LV_EVENT_CLICKED, s);
    }

    s->texts[TEXT_NIGHT] = label(s->rows[ROW_NIGHT], "夜间应答音量");
    s->texts[TEXT_NIGHT_VALUE] = label(s->rows[ROW_NIGHT], "");
    s->texts[TEXT_NIGHT_HELP] = label(s->rows[ROW_NIGHT], "按当前音量的比例；静音优先");
    s->night = lv_slider_create(s->rows[ROW_NIGHT]);
    lv_slider_set_range(s->night, BOT_VOICE_NIGHT_GAIN_MIN, 100);
    lv_obj_set_ext_click_area(s->night, 18);
    lv_obj_add_event_cb(s->night, slider_event, LV_EVENT_VALUE_CHANGED, s);
    lv_obj_add_event_cb(s->night, slider_event, LV_EVENT_RELEASED, s);

    s->preview = button(s->body);
    s->texts[TEXT_PREVIEW] = label(s->preview, "试听一句");
    lv_obj_center(s->texts[TEXT_PREVIEW]);
    lv_obj_add_event_cb(s->preview, preview_clicked, LV_EVENT_CLICKED, s);
    s->defaults = button(s->body);
    s->texts[TEXT_DEFAULT] = label(s->defaults, "恢复默认");
    lv_obj_center(s->texts[TEXT_DEFAULT]);
    lv_obj_add_event_cb(s->defaults, defaults_clicked, LV_EVENT_CLICKED, s);
    s->texts[TEXT_PREVIEW_HELP] = label(s->body, "");
    s->texts[TEXT_NOTICE] = label(s->body, "");
    /* Runtime notices can contain characters beyond this page's fixed labels. */
    lv_obj_set_style_text_font(s->texts[TEXT_NOTICE], &xiaodai_font_sc_30, 0);
    lv_label_set_long_mode(s->texts[TEXT_NOTICE], LV_LABEL_LONG_WRAP);
    s->texts[TEXT_DIAGNOSTIC_TITLE] = label(s->rows[ROW_DIAGNOSTICS], "声音状态 · 只读");
    s->texts[TEXT_DIAGNOSTIC] = label(s->rows[ROW_DIAGNOSTICS], "等待声音状态");
    lv_label_set_long_mode(s->texts[TEXT_DIAGNOSTIC], LV_LABEL_LONG_WRAP);
    lv_obj_set_style_text_line_space(s->texts[TEXT_DIAGNOSTIC], 6, 0);
    face_sound_settings_set_palette(s, lv_color_hex(0x080b11), lv_color_hex(0x15202a),
                                     lv_color_hex(0xffffff), lv_color_hex(0x00d8e8));
    face_sound_settings_layout(s, 800, 480);
    refresh_controls(s);
    return s;
}

void face_sound_settings_destroy(face_sound_settings_t *s) {
    if (!s) return;
    if (s->root) lv_obj_delete(s->root);
    free(s);
}

lv_obj_t *face_sound_settings_root(face_sound_settings_t *s) { return s ? s->root : NULL; }

void face_sound_settings_open(face_sound_settings_t *s) {
    if (!s) return;
    if (s->authoritative_valid) s->value = s->authoritative;
    lv_obj_scroll_to_y(s->body, 0, LV_ANIM_OFF);
    refresh_controls(s);
}

void face_sound_settings_set_palette(face_sound_settings_t *s,
                                     lv_color_t background, lv_color_t tile,
                                     lv_color_t text, lv_color_t accent) {
    if (!s) return;
    s->background = background;
    s->tile = tile;
    s->text = text;
    s->accent = accent;
    paint(s);
}

void face_sound_settings_on_request(face_sound_settings_t *s,
    bool (*callback)(const bot_voice_settings_t *, void *), void *context) {
    if (!s) return;
    s->on_request = callback;
    s->request_context = context;
    refresh_controls(s);
}

void face_sound_settings_on_preview(face_sound_settings_t *s,
    bool (*callback)(void *), void *context) {
    if (!s) return;
    s->on_preview = callback;
    s->preview_context = context;
    refresh_controls(s);
}

void face_sound_settings_set_value(face_sound_settings_t *s, const bot_voice_settings_t *value) {
    if (!s || !bot_voice_settings_valid(value)) return;
    if (s->authoritative_valid && bot_voice_settings_equal(&s->authoritative, value)) return;
    s->authoritative = *value;
    s->authoritative_valid = true;
    s->value = *value;
    refresh_controls(s);
}

static bool diagnostics_equal(const bot_voice_diagnostics_t *a, const bot_voice_diagnostics_t *b) {
    /* Compare the rendered metadata, not struct padding or unrendered counters. */
    return a->mic_available == b->mic_available && a->capture_ready == b->capture_ready &&
        a->model_ready == b->model_ready && a->link_ready == b->link_ready &&
        a->playback_blocked == b->playback_blocked && a->settings_applied == b->settings_applied &&
        a->wake_score_available == b->wake_score_available &&
        (!a->wake_score_available || a->last_wake_score == b->last_wake_score) &&
        a->peak0 == b->peak0 && a->peak1 == b->peak1 && a->wake_count == b->wake_count &&
        a->local_command_count == b->local_command_count && a->read_errors == b->read_errors &&
        a->playback_underruns == b->playback_underruns && a->buffered_drops == b->buffered_drops &&
        a->last_error == b->last_error;
}

void face_sound_settings_set_diagnostics(face_sound_settings_t *s,
                                         const bot_voice_diagnostics_t *value) {
    if (!s || !value) return;
    if (s->diagnostics_valid && diagnostics_equal(&s->diagnostics, value)) return;
    s->diagnostics = *value;
    s->diagnostics_valid = true;
    const char *mic = value->mic_available ? "可用" : "不可用";
    const char *wake = value->model_ready ? "就绪" : "未就绪";
    const char *link = value->link_ready ? "已连接" : "未连接";
    const char *capture = value->capture_ready ? "就绪" : "关闭";
    const char *self_wake = value->playback_blocked ? "进行中" : "待机";
    char score[40];
    /* Avoid requiring LVGL's optional printf float support. This is a model
     * score for diagnosis, not an accuracy percentage or wake authorization. */
    if (value->wake_score_available && value->last_wake_score >= 0.0f &&
        value->last_wake_score <= 1000.0f) {
        const unsigned scaled = (unsigned)(value->last_wake_score * 100.0f + 0.5f);
        lv_snprintf(score, sizeof(score), "%u.%02u", scaled / 100, scaled % 100);
    } else lv_snprintf(score, sizeof(score), "暂无");
    lv_label_set_text_fmt(s->texts[TEXT_DIAGNOSTIC],
        "麦克风：%s    唤醒模型：%s\n"
        "Mac：%s    对话收音：%s\n"
        "声音设置：%s    防自唤醒：%s\n"
        "本次启动：唤醒 %lu 次，指令 %lu 次\n"
        "输入峰值：%d / %d    最近唤醒得分：%s\n"
        "读取错误：%lu    播放缺口：%lu\n"
        "缓冲丢弃：%lu    最近错误：%ld\n"
        "完整唤醒词：小呆小呆",
        mic, wake, link, capture, value->settings_applied ? "已应用" : "待应用", self_wake,
        (unsigned long)value->wake_count, (unsigned long)value->local_command_count,
        (int)value->peak0, (int)value->peak1, score,
        (unsigned long)value->read_errors, (unsigned long)value->playback_underruns,
        (unsigned long)value->buffered_drops, (long)value->last_error);
}

void face_sound_settings_layout(face_sound_settings_t *s, int w, int h) {
    if (!s || w < 350 || h < 240) return;
    put(s->root, 0, 0, w, h);
    put(s->back, 37, 28, 52, 47);
    put(s->texts[TEXT_TITLE], 110, 43, w - 150, 30);
    const int margin = w * 4 / 100;
    const int width = w - 2 * margin - 8; /* reserve scrollbar space */
    put(s->body, margin, 100, w - 2 * margin, h - 116);
    put(s->rows[ROW_VOLUME], 0, 0, width, 108);
    put(s->texts[TEXT_VOLUME], 22, 18, width - 140, 30);
    put(s->texts[TEXT_VOLUME_VALUE], width - 104, 18, 82, 30);
    put(s->volume, 34, 78, width - 68, 12);
    put(s->rows[ROW_MUTE], 0, 120, width, 76);
    put(s->texts[TEXT_MUTE], 22, 24, 100, 30);
    put(s->texts[TEXT_MUTE_HELP], 150, 24, width - 280, 30);
    put(s->mute, width - 96, 19, 72, 38);
    put(s->rows[ROW_MODE], 0, 208, width, 104);
    put(s->texts[TEXT_MODE], 22, 18, 180, 30);
    put(s->texts[TEXT_MODE_VALUE], width - 270, 18, 160, 30);
    put(s->mode, width - 96, 14, 72, 38);
    put(s->texts[TEXT_MODE_HELP], 22, 64, width - 44, 30);
    put(s->rows[ROW_WAKE], 0, 324, width, 110);
    put(s->texts[TEXT_WAKE], 22, 22, 200, 30);
    const int choice_width = (width - 280) / 3;
    for (size_t i = 0; i < 3; ++i)
        put(s->wake_choices[i], 240 + (int)i * (choice_width + 8), 14, choice_width, 46);
    put(s->texts[TEXT_WAKE_HELP], 22, 72, width - 44, 30);
    put(s->rows[ROW_NIGHT], 0, 446, width, 138);
    put(s->texts[TEXT_NIGHT], 22, 18, width - 140, 30);
    put(s->texts[TEXT_NIGHT_VALUE], width - 104, 18, 82, 30);
    put(s->texts[TEXT_NIGHT_HELP], 22, 54, width - 44, 30);
    put(s->night, 34, 108, width - 68, 12);
    const int action_width = (width - 14) / 2;
    put(s->preview, 0, 600, action_width, 56);
    put(s->defaults, action_width + 14, 600, action_width, 56);
    put(s->texts[TEXT_PREVIEW_HELP], 2, 670, width - 4, 30);
    put(s->texts[TEXT_NOTICE], 2, 710, width - 4, 120);
    put(s->rows[ROW_DIAGNOSTICS], 0, 842, width, 344);
    put(s->texts[TEXT_DIAGNOSTIC_TITLE], 22, 18, width - 44, 30);
    put(s->texts[TEXT_DIAGNOSTIC], 22, 62, width - 44, 270);
}
