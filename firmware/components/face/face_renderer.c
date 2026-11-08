#include "face_renderer.h"
#include <stdlib.h>
#include <string.h>

extern const face_renderer_driver_t face_outline_renderer_driver;
extern const face_renderer_driver_t face_pill_renderer_driver;
extern const face_renderer_driver_t face_eve_renderer_driver;

/* Compile-time registry is intentional: no dynamic code loading on a microcontroller. */
static const face_renderer_driver_t *const DRIVERS[] = {
    &face_outline_renderer_driver,
    &face_pill_renderer_driver,
    &face_eve_renderer_driver,
};

struct face_renderer {
    const face_renderer_driver_t *driver;
    void *instance;
};

static const face_renderer_driver_t *find_driver(const char *id) {
    if (!id) return NULL;
    for (size_t i = 0; i < sizeof(DRIVERS) / sizeof(DRIVERS[0]); i++) {
        if (strcmp(id, DRIVERS[i]->id) == 0) return DRIVERS[i];
    }
    return NULL;
}

bool face_renderer_supported(const char *id) {
    return find_driver(id) != NULL;
}

face_renderer_t *face_renderer_create(const char *id, lv_obj_t *parent) {
    if (!parent) return NULL;
    const face_renderer_driver_t *driver = find_driver(id);
    if (!driver) return NULL;
    face_renderer_t *f = calloc(1, sizeof(*f));
    if (!f) return NULL;
    f->instance = driver->create(parent);
    if (!f->instance) {
        free(f);
        return NULL;
    }
    f->driver = driver;
    return f;
}

void face_renderer_render(face_renderer_t *f, const face_theme_t *theme,
                          const face_render_state_t *state, int width, int height) {
    if (!f || !theme || !state || width < 60 || height < 40) return;
    f->driver->render(f->instance, theme, state, width, height);
}

void face_renderer_destroy(face_renderer_t *f) {
    if (!f) return;
    f->driver->destroy(f->instance);
    free(f);
}

const char *face_renderer_id(const face_renderer_t *f) {
    return f ? f->driver->id : NULL;
}

face_expression_t face_renderer_legacy_expression(face_expression_t ex) {
    switch (ex) {
        case FACE_EX_GENTLE:
        case FACE_EX_DETERMINED: return FACE_EX_LISTENING;
        case FACE_EX_CONFUSED: return FACE_EX_CURIOUS;
        case FACE_EX_DISCOVERY: return FACE_EX_SURPRISED;
        case FACE_EX_LAUGHING:
        case FACE_EX_AFFECTION:
        case FACE_EX_RELIEVED:
        case FACE_EX_OVERJOYED:
        case FACE_EX_EXCITED: return FACE_EX_HAPPY;
        case FACE_EX_WINK:
        case FACE_EX_SKEPTICAL: return FACE_EX_THINKING;
        case FACE_EX_SMUG:
        case FACE_EX_WORRIED: return FACE_EX_SHY;
        case FACE_EX_SAD: return FACE_EX_SLEEPY;
        case FACE_EX_ANNOYED: return FACE_EX_BORED;
        default: return ex;
    }
}
