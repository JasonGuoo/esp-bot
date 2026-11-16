/* Authenticated device media for the Mac v3 gateway. Wi-Fi discovery remains
 * diagnostic only. The runtime owns wake, cancellation and all permissions. */
#include "brain_link.h"
#include "bot_lan.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdatomic.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/socket.h>
#include <arpa/inet.h>
#include "cJSON.h"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "http_parser.h"
#include "esp_tls_errors.h"
#include "peer_path.h"
#include "esp_websocket_client.h"
#include "freertos/queue.h"
#include "freertos/idf_additions.h"
#include "freertos/task.h"
#include "mbedtls/base64.h"
#include "mbedtls/md.h"
#include "esp_jpeg_dec.h"
#include "nvs.h"
#include "sdkconfig.h"

static const char *TAG = "brain_secure";
enum { URL_SIZE = 256, CERT_SIZE = 2048, TOKEN_SIZE = 65, RX_SIZE = 8192,
       PCM_SAMPLES = 320, PCM_QUEUE_FRAMES = 96, PLAIN_RX_SIZE = 4096, PLAIN_MAC_SIZE = 32,
       IMAGE_ERROR_CODE_BYTES = 40, MAX_REPLY_PCM_BYTES = 120 * 16000 * 2 };
typedef struct { uint32_t version; char url[URL_SIZE], cert[CERT_SIZE], token[TOKEN_SIZE]; } provision_t;
typedef struct { bot_session_token_t token; uint64_t timestamp_ms; uint16_t count; int16_t pcm[PCM_SAMPLES]; } pcm_frame_t;
typedef enum { DOWN, WELCOME, READY, OPENING, OPEN, BEGINNING, PLAIN_CONNECTING,
    CAPTURING, COMMITTED, PLAYBACK_PENDING, PLAYING, DRAINING, WAIT_NEXT } phase_t;
typedef struct {
    phase_t phase;
    bool authenticated, active, utterance, audio_allowed, pcm_permitted, silent_supported;
    bool audio_supported, spoken_answers, session_spoken, followup_armed;
    bool presentation_supported, presentation_valid, segment_active, segment_final;
    bool segment_text_done, segment_image_done, segment_audio_done, segment_failed, segment_video;
    uint32_t presentation_sequence, presentation_index, segment_generation, presented_segments, segment_hold_ms;
    int64_t presentation_started_us, segment_started_us, segment_rendered_us, video_started_us;
    bot_playback_purpose_t playback_purpose;
    char presentation_id[65], output_lease_id[65], segment_id[65];
    char segment_asset_id[BOT_DISPLAY_ASSET_ID_BYTES], segment_code[40];
    bool result_valid, result_admitted, playback_cancel_requested, cancel_before_next;
    bot_activation_t activation;
    bot_session_token_t desired, remote, result_token, playback_token;
    uint32_t stream, sequence, playback_id, playback_stream, playback_sequence;
    uint32_t playback_expected, playback_received;
    uint8_t reply_gain_percent, playback_gain_percent;
    int64_t playback_started_us;
    uint32_t progress_seq;
    int64_t commit_started_us;
    char open_id[24], begin_id[24], commit_id[24];
    char image_request_id[24];
    char image_asset_id[BOT_DISPLAY_ASSET_ID_BYTES];
    char plain_host[16];
    uint16_t plain_port;
    uint8_t plain_key[32];
} secure_state_t;
static portMUX_TYPE s_lock = portMUX_INITIALIZER_UNLOCKED;
static secure_state_t s;
static provision_t s_provision;
static char s_tls_name[254], s_ca_fingerprint[65], s_dial_uri[URL_SIZE];
/* Owned by the WebSocket event task. */
static char s_peer_ip[16];
static bool s_via_gateway, s_switch_path, s_connect_pending;
static int64_t s_connect_us;
static atomic_bool s_discovery_enabled;
static bot_bus_t *s_bus;
static bot_audio_t *s_audio;
static QueueHandle_t s_pcm;
static esp_websocket_client_handle_t s_ws;
static atomic_bool s_overflow;
static bot_session_token_t s_overflow_token; /* s_lock */
static atomic_bool s_playback_event_overflow;
typedef struct {
    bot_audio_event_kind_t kind;
    bot_session_token_t token;
    uint32_t playback_id, stream, frames, bytes;
    uint32_t presentation_sequence;
    esp_err_t error;
} playback_event_t;
static QueueHandle_t s_playback_events;
static char *s_rx;
static uint8_t *s_pcm_storage;
static StaticQueue_t s_pcm_control;
static size_t s_rx_used;
static int s_rx_opcode;
static uint32_t s_request;
/* Owned exclusively by secure_task; the WSS callback only supplies the
 * per-turn destination/key under s_lock. PCM is intentionally plaintext. */
static int s_plain_fd = -1;
static uint8_t s_plain_key[32];
static uint8_t *s_plain_rx;
static size_t s_plain_used;
static int64_t s_plain_blocked_us;
typedef enum { IMAGE_DESCRIPTOR, IMAGE_CHUNK } image_message_kind_t;
typedef struct {
    image_message_kind_t kind;
    bot_session_token_t token;
    char asset_id[BOT_DISPLAY_ASSET_ID_BYTES], digest[65];
    uint32_t total, offset, bytes;
    uint32_t presentation_sequence;
    uint16_t width, height;
    bool eof;
    char base64[4097];
} image_message_t;
typedef struct {
    bot_session_token_t token;
    char part[8], asset_id[BOT_DISPLAY_ASSET_ID_BYTES];
    uint32_t presentation_sequence;
    char code[IMAGE_ERROR_CODE_BYTES];
    bool rendered;
} display_report_t;
typedef struct {
    bot_session_token_t token;
    char asset_id[BOT_DISPLAY_ASSET_ID_BYTES], code[IMAGE_ERROR_CODE_BYTES];
    uint32_t generation;
    bool pending, local_posted;
} image_failure_t;
static QueueHandle_t s_image_messages, s_image_results, s_display_reports;
/* A single current image is supported. Keep failure reporting independent of
 * image allocations and queue capacity, including after an allocation fails. */
static image_failure_t s_image_failure; /* s_lock */
static bool same(bot_session_token_t a, bot_session_token_t b);
static bool asset_id_valid(const char *id);

static bool image_error_code_valid(const char *code) {
    if (!code || !*code || strlen(code)>=IMAGE_ERROR_CODE_BYTES) return false;
    for (const char *p=code;*p;p++)
        if (!((*p>='a'&&*p<='z')||(*p>='0'&&*p<='9')||*p=='_')) return false;
    return true;
}

bool brain_secure_take_image(bot_display_image_t *image) {
    return image && s_image_results && xQueueReceive(s_image_results,image,0)==pdTRUE;
}
bool brain_secure_display_report_sequence(bot_session_token_t token, uint32_t sequence,
                                          const char *part, const char *asset_id,
                                          bool rendered, const char *code) {
    if (!s_display_reports || !part || (strcmp(part,"text") && strcmp(part,"image")) ||
        (asset_id && *asset_id && !asset_id_valid(asset_id)) ||
        (!rendered && !image_error_code_valid(code))) return false;
    display_report_t report={.token=token,.rendered=rendered,.presentation_sequence=sequence};
    snprintf(report.part,sizeof(report.part),"%s",part);
    if (asset_id) snprintf(report.asset_id,sizeof(report.asset_id),"%s",asset_id);
    if (code) snprintf(report.code,sizeof(report.code),"%s",code);
    return xQueueSend(s_display_reports,&report,0)==pdTRUE;
}

bool brain_secure_display_report(bot_session_token_t token, const char *part,
                                 const char *asset_id, bool rendered, const char *code) {
    return brain_secure_display_report_sequence(token,0,part,asset_id,rendered,code);
}

static bool same(bot_session_token_t a, bot_session_token_t b) {
    return a.epoch == b.epoch && a.session_id == b.session_id && a.turn_id == b.turn_id &&
           a.focus_generation == b.focus_generation;
}
void brain_secure_image_failed(bot_session_token_t token, const char *asset_id, const char *code) {
    image_failure_t failure={.token=token,.pending=true};
    if (asset_id_valid(asset_id)) snprintf(failure.asset_id,sizeof(failure.asset_id),"%s",asset_id);
    snprintf(failure.code,sizeof(failure.code),"%s",image_error_code_valid(code)?code:"image_request_failed");
    portENTER_CRITICAL(&s_lock);
    if (s.authenticated && s.result_valid && same(token,s.result_token) &&
        (!s.presentation_valid || (s.segment_active && asset_id &&
         !strcmp(asset_id,s.segment_asset_id))) &&
        (!s_image_failure.generation || !same(token,s_image_failure.token))) {
        failure.generation=s_image_failure.generation+1;
        if (!failure.generation) failure.generation=1;
        s_image_failure=failure;
        if (s.presentation_valid && s.segment_active) {
            s.segment_image_done=true; s.segment_failed=true;
            snprintf(s.segment_code,sizeof(s.segment_code),"%s",failure.code);
        }
    }
    portEXIT_CRITICAL(&s_lock);
}
static bool image_failed_for(bot_session_token_t token) {
    portENTER_CRITICAL(&s_lock);
    bool failed=s_image_failure.generation && same(token,s_image_failure.token);
    portEXIT_CRITICAL(&s_lock);
    return failed;
}
static bool image_current(bot_session_token_t token) {
    portENTER_CRITICAL(&s_lock);
    bool current=s.authenticated && s.result_valid && same(token,s.result_token);
    portEXIT_CRITICAL(&s_lock);
    return current;
}
static void link_event(bool online) {
    if (!s_bus) return;
    bot_event_t e = {.kind=BOT_EVENT_LINK_STATE, .source=BOT_SOURCE_LINK,
        .observed_ms=(uint32_t)(esp_timer_get_time()/1000),
        .data.link={.connected=online,.authenticated=online,.encrypted=online}};
    if (bot_bus_post(s_bus, &e, 0) != ESP_OK) bot_bus_request_stop(s_bus, e.observed_ms);
}
static void capture_event(bot_session_token_t token, bool ready) {
    if (!s_bus || !token.session_id) return;
    const bot_event_t event={.kind=BOT_EVENT_CAPTURE_READY,.source=BOT_SOURCE_LINK,
        .observed_ms=(uint32_t)(esp_timer_get_time()/1000),.token=token,
        .data.capture={.ready=ready}};
    (void)bot_bus_post(s_bus,&event,0);
}
static void fail_session_token(const char *reason, const bot_session_token_t *expected,
                                bot_session_end_code_t code) {
    portENTER_CRITICAL(&s_lock);
    if (expected && !same(*expected,s.desired)) {portEXIT_CRITICAL(&s_lock);return;}
    s.audio_allowed = false;
    s.phase=s.authenticated?WAIT_NEXT:DOWN;
    bool active = s.active;
    const bot_session_token_t token = s.desired;
    const bot_session_token_t remote = s.remote;
    const uint32_t playback = s.playback_id;
    s.playback_cancel_requested = playback != 0;
    portEXIT_CRITICAL(&s_lock);
    ESP_LOGW(TAG, "%s", reason);
    /* Transport owns only its own playback lease. A disconnect or late error
     * must not cancel an independent local wake acknowledgement. */
    if (s_audio && playback) bot_audio_playback_cancel(s_audio, playback);
    capture_event(remote,false);
    if (active && s_bus) {
        const bot_event_t event = {.kind = BOT_EVENT_SESSION_END, .source = BOT_SOURCE_SYSTEM,
            .observed_ms = (uint32_t)(esp_timer_get_time()/1000), .token = token,
            .data.session_end = {.code=code}};
        /* Token matching in runtime makes a concurrent new wake immune to this
         * old failure. A critical enqueue failure still latches the bus fault. */
        (void)bot_bus_post(s_bus, &event, 0);
    }
}
static void fail_session(const char *reason) { fail_session_token(reason,NULL,BOT_SESSION_END_NONE); }
static void fail_operation(bot_session_token_t token,const char *reason) { fail_session_token(reason,&token,BOT_SESSION_END_NONE); }
void brain_secure_network_down(void) {
    portENTER_CRITICAL(&s_lock);
    bool was = s.authenticated;
    s.authenticated = false; s.audio_allowed = false; s.result_valid=false; s.phase = DOWN;
    portEXIT_CRITICAL(&s_lock);
    if (was) { link_event(false); fail_session("Secure connection lost; session stopped"); }
}

static bool no_control(const char *s) {
    for (; *s; ++s) if ((unsigned char)*s < 0x20 || (unsigned char)*s > 0x7e) return false;
    return true;
}
static bool valid_provision(const provision_t *p) {
    if (p->version != 1 || !memchr(p->url,0,sizeof(p->url)) || !memchr(p->cert,0,sizeof(p->cert)) ||
        !memchr(p->token,0,sizeof(p->token))) return false;
    const char *suffix = "/v3/brain/ws";
    size_t len = strlen(p->url), tail = strlen(suffix);
    if (len <= 6 + tail || strncmp(p->url,"wss://",6) || strcmp(p->url+len-tail,suffix) ||
        !no_control(p->url) || strchr(p->url,'@') || strchr(p->url,'?') || strchr(p->url,'#')) return false;
    if (strncmp(p->cert,"-----BEGIN CERTIFICATE-----",27) || !strstr(p->cert,"-----END CERTIFICATE-----")) return false;
    if (strlen(p->token) != 64) return false;
    for (size_t i=0;i<64;i++) if (!((p->token[i]>='0'&&p->token[i]<='9')||(p->token[i]>='a'&&p->token[i]<='f'))) return false;
    return true;
}
esp_err_t brain_secure_provision(const char *input) {
    if (!input || strlen(input)>3900) return ESP_ERR_INVALID_ARG;
    size_t size=0; unsigned char *decoded=calloc(1, 3001);
    if (!decoded) return ESP_ERR_NO_MEM;
    esp_err_t result=ESP_ERR_INVALID_ARG;
    if (mbedtls_base64_decode(decoded,3000,&size,(const unsigned char *)input,strlen(input)) != 0 || !size || size>3000) goto done;
    cJSON *json=cJSON_ParseWithLength((const char *)decoded,size);
    if (!json) goto done;
    cJSON *url=cJSON_GetObjectItemCaseSensitive(json,"url"), *cert=cJSON_GetObjectItemCaseSensitive(json,"certificate"),
          *token=cJSON_GetObjectItemCaseSensitive(json,"token");
    provision_t *p=calloc(1,sizeof(*p));
    if (!p) { cJSON_Delete(json); result=ESP_ERR_NO_MEM; goto done; }
    p->version=1;
    if (!cJSON_IsString(url) || !cJSON_IsString(cert) || !cJSON_IsString(token) ||
        strlen(url->valuestring)>=sizeof(p->url) || strlen(cert->valuestring)>=sizeof(p->cert) ||
        strlen(token->valuestring)>=sizeof(p->token)) goto parsed;
    strcpy(p->url,url->valuestring); strcpy(p->cert,cert->valuestring); strcpy(p->token,token->valuestring);
    if (valid_provision(p)) {
        nvs_handle_t nvs;
        result=nvs_open("espbot_secure",NVS_READWRITE,&nvs);
        if (result==ESP_OK) {
            result=nvs_set_blob(nvs,"server",p,sizeof(*p));
            if (result==ESP_OK) result=nvs_commit(nvs);
            nvs_close(nvs);
        }
    }
parsed:
    memset(p,0,sizeof(*p)); free(p); cJSON_Delete(json);
done:
    memset(decoded,0,3001); free(decoded); return result;
}

void brain_secure_update(const bot_runtime_snapshot_t *state) {
    if (!state) return;
    portENTER_CRITICAL(&s_lock);
    s.active=state->audio_session_requested; s.utterance=state->utterance_active;
    s.activation=state->activation; s.desired=state->token;
    s.pcm_permitted=state->audio_uplink_permitted;
    s.followup_armed=state->followup_armed;
    s.spoken_answers=state->voice_settings.spoken_answers;
    s.reply_gain_percent=state->clock_valid && state->night_policy?state->voice_settings.night_gain_percent:100;
    if (!s.active || !same(s.desired,s.remote)) {
        s.audio_allowed=false;
        /* Revoke media immediately; later buffers retain the old token/seq. */
        s.presentation_valid=false; s.segment_active=false;
        s.presentation_index=s.segment_generation=s.presented_segments=0;
    }
    if (s.result_valid) {
        if (same(state->reading_token,s.result_token)) s.result_admitted=true;
        // Admission may lag the WSS callback by one runtime tick. A prepared
        // follow-up has a new capture token but still owns the previous result.
        if ((s.result_admitted && !same(state->reading_token,s.result_token)) ||
            (!same(state->token,s.result_token) && !state->followup_armed)) {
            s.result_valid=false;
        }
    }
    portEXIT_CRITICAL(&s_lock);
}
bool brain_secure_uplink(const bot_session_token_t *token) {
    if (!token) return false;
    portENTER_CRITICAL(&s_lock);
    bool yes=s.authenticated && s.active && s.audio_allowed && s.phase==CAPTURING && same(*token,s.desired) && same(*token,s.remote);
    portEXIT_CRITICAL(&s_lock);
    return yes;
}
void brain_secure_pcm(const bot_audio_pcm_t *frame) {
    if (!frame || !s_pcm || frame->sample_rate!=16000) return;
    bot_session_token_t token={frame->token.epoch,frame->token.session_id,frame->token.turn_id,frame->token.focus_generation};
    if (!brain_secure_uplink(&token)) return;
    portENTER_CRITICAL(&s_lock);
    bool permitted=s.pcm_permitted && same(token,s.desired);
    portEXIT_CRITICAL(&s_lock);
    if (!permitted) return;
    for (size_t at=0; at<frame->sample_count;) {
        size_t n=frame->sample_count-at; if (n>PCM_SAMPLES) n=PCM_SAMPLES;
        pcm_frame_t packet={.token=token,.timestamp_ms=(uint64_t)(frame->observed_us/1000),.count=(uint16_t)n};
        memcpy(packet.pcm,frame->samples+at,n*sizeof(int16_t));
        if (xQueueSend(s_pcm,&packet,0)!=pdTRUE) {
            portENTER_CRITICAL(&s_lock);
            if (same(token,s.desired)) {s_overflow_token=token;atomic_store(&s_overflow,true);}
            portEXIT_CRITICAL(&s_lock);
            return;
        }
        at+=n;
    }
}

void brain_secure_audio_event(const bot_audio_event_t *event) {
    if (!event || !s_playback_events || event->playback_source!=BOT_AUDIO_PLAYBACK_SESSION ||
        (event->kind!=BOT_AUDIO_EVENT_PLAYBACK_STARTED && event->kind!=BOT_AUDIO_EVENT_PLAYBACK_DONE &&
         event->kind!=BOT_AUDIO_EVENT_PLAYBACK_CANCELLED && event->kind!=BOT_AUDIO_EVENT_PLAYBACK_FAILED)) return;
    const bot_session_token_t token={event->token.epoch,event->token.session_id,
        event->token.turn_id,event->token.focus_generation};
    portENTER_CRITICAL(&s_lock);
    bool current=event->playback_id && same(token,s.playback_token) &&
        (event->playback_id==s.playback_id || (!s.playback_id && s.phase==PLAYBACK_PENDING));
    playback_event_t copied={.kind=event->kind,.token=token,.playback_id=event->playback_id,
        .stream=s.playback_stream,.frames=s.playback_sequence,.bytes=s.playback_received,.error=event->error,
        .presentation_sequence=event->presentation_managed?s.segment_generation:0};
    portEXIT_CRITICAL(&s_lock);
    if (current && xQueueSend(s_playback_events,&copied,0)!=pdTRUE)
        atomic_store(&s_playback_event_overflow,true);
}

static void command(bot_command_kind_t kind, bot_session_token_t token, const char *text) {
    bot_command_t c={.kind=kind,.token=token,.received_ms=(uint32_t)(esp_timer_get_time()/1000),.ttl_ms=2000};
    if (kind==BOT_COMMAND_PRESENTATION_SEGMENT) {
        portENTER_CRITICAL(&s_lock); c.presentation_sequence=s.segment_generation; portEXIT_CRITICAL(&s_lock);
    }
    if (text && kind==BOT_COMMAND_SET_EXPRESSION) snprintf(c.data.expression,sizeof(c.data.expression),"%s",text);
    if (text && (kind==BOT_COMMAND_SET_CAPTION || kind==BOT_COMMAND_FAILURE_CAPTION ||
        kind==BOT_COMMAND_REPLY_DONE || kind==BOT_COMMAND_REPLY_PRESENT || kind==BOT_COMMAND_PRESENTATION_SEGMENT)) {
        size_t n=strlen(text); if (n>BOT_CAPTION_MAX_BYTES) n=BOT_CAPTION_MAX_BYTES;
        while (n && ((unsigned char)text[n]&0xc0)==0x80) --n;
        memcpy(c.data.caption,text,n); c.data.caption[n]=0;
    }
    if (bot_bus_submit_command(s_bus,&c,0)!=ESP_OK) fail_operation(token,"Secure response queue full");
}
static void progress_command(bot_session_token_t token, bool waiting_for_approval) {
    bot_command_t c={.kind=BOT_COMMAND_PROGRESS,.token=token,
        .received_ms=(uint32_t)(esp_timer_get_time()/1000),.ttl_ms=2000};
    c.data.waiting_for_approval=waiting_for_approval;
    if (bot_bus_submit_command(s_bus,&c,0)!=ESP_OK) fail_operation(token,"Secure response queue full");
}
static bool array_has_string(cJSON *array, const char *value) {
    cJSON *item;
    cJSON_ArrayForEach(item,array) {
        if (cJSON_IsString(item) && !strcmp(item->valuestring,value)) return true;
    }
    return false;
}
static const char *request_error(cJSON *root) {
    cJSON *error=cJSON_GetObjectItemCaseSensitive(root,"error");
    cJSON *code=cJSON_GetObjectItemCaseSensitive(error,"code");
    const char *name=cJSON_IsString(code)?code->valuestring:"";
    if (!strcmp(name,"asr_no_speech")) return "没听清，请再说一次";
    if (!strcmp(name,"brain_busy")) return "小呆正在处理别的事，请稍后再试";
    if (!strcmp(name,"brain_not_started")) return "请先在 Mac 上启动小呆服务";
    if (!strcmp(name,"display_output_unsupported")) return "请更新 Mac 小呆服务";
    if (!strcmp(name,"turn_not_replayed")) return "执行结果不确定，请在 Mac 上检查";
    return "连接或处理失败，请在 Mac 上检查";
}
static void finish_with_message(bot_session_token_t token, const char *message, bool reply_done) {
    portENTER_CRITICAL(&s_lock);
    if (!s.authenticated || !s.active || !same(s.desired,token)) {
        portEXIT_CRITICAL(&s_lock);
        return;
    }
    s.audio_allowed=false;
    s.remote=token;
    s.phase=WAIT_NEXT;
    portEXIT_CRITICAL(&s_lock);
    capture_event(token,false);
    command(BOT_COMMAND_FAILURE_CAPTION,token,message);
    if (reply_done) command(BOT_COMMAND_REPLY_DONE,token,NULL);
}
static bool json_token(cJSON *value, bot_session_token_t *out) {
    if (!cJSON_IsObject(value)) return false;
    cJSON *e=cJSON_GetObjectItemCaseSensitive(value,"epoch"),*s_id=cJSON_GetObjectItemCaseSensitive(value,"session_id"),
          *t=cJSON_GetObjectItemCaseSensitive(value,"turn_id"),*f=cJSON_GetObjectItemCaseSensitive(value,"focus_generation");
    cJSON *fields[]={e,s_id,t,f}; uint32_t nums[4];
    for (unsigned i=0;i<4;i++) {
        if (!cJSON_IsNumber(fields[i]) || fields[i]->valuedouble<0 || fields[i]->valuedouble>4294967295.0) return false;
        nums[i]=(uint32_t)fields[i]->valuedouble;
        if ((double)nums[i]!=fields[i]->valuedouble) return false;
    }
    *out=(bot_session_token_t){nums[0],nums[1],nums[2],nums[3]}; return true;
}
static bool decode_key(const char *hex,uint8_t out[32]);
static bool json_uint(cJSON *value, uint32_t maximum, uint32_t *out) {
    if (!cJSON_IsNumber(value) || value->valuedouble<0 || value->valuedouble>maximum) return false;
    uint32_t n=(uint32_t)value->valuedouble;
    if ((double)n!=value->valuedouble) return false;
    *out=n;
    return true;
}
static bool asset_id_valid(const char *id) {
    if (!id || !*id || strlen(id)>=BOT_DISPLAY_ASSET_ID_BYTES) return false;
    for (const unsigned char *p=(const unsigned char *)id;*p;p++)
        if (!((*p>='a'&&*p<='z')||(*p>='A'&&*p<='Z')||(*p>='0'&&*p<='9')||*p=='-'||*p=='_')) return false;
    return true;
}
static void queue_image_message(image_message_t *message) {
    if (!message) return;
    if (!s_image_messages || xQueueSend(s_image_messages,&message,0)!=pdTRUE) {
        brain_secure_image_failed(message->token,message->asset_id,"image_queue_full");
        free(message);
    }
}
static void queue_image_descriptor(cJSON *scene, bot_session_token_t token) {
    cJSON *cards=cJSON_GetObjectItemCaseSensitive(scene,"cards"), *card;
    cJSON_ArrayForEach(card,cards) {
        cJSON *kind=cJSON_GetObjectItemCaseSensitive(card,"kind");
        cJSON *asset=cJSON_GetObjectItemCaseSensitive(card,"asset");
        cJSON *id=cJSON_GetObjectItemCaseSensitive(asset,"id"), *mime=cJSON_GetObjectItemCaseSensitive(asset,"mime");
        cJSON *digest=cJSON_GetObjectItemCaseSensitive(asset,"sha256");
        uint32_t width,height,total;
        if (!cJSON_IsString(kind) || strcmp(kind->valuestring,"image")) continue;
        const char *asset_id=cJSON_IsString(id) && asset_id_valid(id->valuestring)?id->valuestring:NULL;
        if (!cJSON_IsString(id) ||
            !asset_id_valid(id->valuestring) || !cJSON_IsString(mime) || strcmp(mime->valuestring,"image/jpeg") ||
            !cJSON_IsString(digest) || strlen(digest->valuestring)!=64 ||
            !json_uint(cJSON_GetObjectItemCaseSensitive(asset,"width"),480,&width) || !width ||
            !json_uint(cJSON_GetObjectItemCaseSensitive(asset,"height"),272,&height) || !height ||
            !json_uint(cJSON_GetObjectItemCaseSensitive(asset,"bytes"),65536,&total) || !total) {
            brain_secure_image_failed(token,asset_id,"image_descriptor_invalid");
            return;
        }
        uint8_t checked_digest[32];
        if (!decode_key(digest->valuestring,checked_digest)) {
            brain_secure_image_failed(token,asset_id,"image_descriptor_invalid");
            return;
        }
        image_message_t *message=heap_caps_calloc(1,sizeof(*message),MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
        if (!message) {
            brain_secure_image_failed(token,asset_id,"image_no_memory");
            return;
        }
        message->kind=IMAGE_DESCRIPTOR;message->token=token;
        portENTER_CRITICAL(&s_lock);
        message->presentation_sequence=s.presentation_valid?s.segment_generation:0;
        portEXIT_CRITICAL(&s_lock);message->width=width;message->height=height;message->total=total;
        snprintf(message->asset_id,sizeof(message->asset_id),"%s",id->valuestring);
        snprintf(message->digest,sizeof(message->digest),"%s",digest->valuestring);
        queue_image_message(message);
        return;
    }
}
static uint32_t read32(const unsigned char *p) {
    return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24);
}
static void write32(unsigned char *p,uint32_t n) { for (unsigned i=0;i<4;i++) p[i]=(unsigned char)(n>>(8*i)); }
static bool decode_key(const char *hex,uint8_t out[32]) {
    if (!hex || strlen(hex)!=64) return false;
    for (size_t i=0;i<32;i++) {
        unsigned char a=(unsigned char)hex[i*2], b=(unsigned char)hex[i*2+1];
        if (!((a>='0'&&a<='9')||(a>='a'&&a<='f')) ||
            !((b>='0'&&b<='9')||(b>='a'&&b<='f'))) return false;
        unsigned hi=a<='9'?a-'0':a-'a'+10, lo=b<='9'?b-'0':b-'a'+10;
        out[i]=(uint8_t)((hi<<4)|lo);
    }
    return true;
}
/* Only secure_task consumes the authenticated TCP stream. A full hardware
 * queue leaves the original frame unconsumed for a bounded retry. */
static esp_err_t playback_frame(const unsigned char *data,size_t n) {
    if (n<22 || n>660 || memcmp(data,"XDAI",4) || (n-20)%2) return ESP_ERR_INVALID_ARG;
    uint32_t stream=read32(data+4), seq=read32(data+8), playback;
    uint64_t timestamp=0;
    for (unsigned i=0;i<8;i++) timestamp|=(uint64_t)data[12+i]<<(8*i);
    portENTER_CRITICAL(&s_lock);
    bool valid=s.authenticated && s.active && s.phase==PLAYING && same(s.desired,s.remote) &&
        stream==s.playback_stream && seq==s.playback_sequence && timestamp==(uint64_t)seq*20 &&
        s.playback_received<=s.playback_expected && n-20<=s.playback_expected-s.playback_received &&
        (n==660 || n-20==s.playback_expected-s.playback_received);
    playback=s.playback_id;
    uint8_t gain=s.playback_gain_percent;
    portEXIT_CRITICAL(&s_lock);
    if (!valid || !playback) return ESP_ERR_INVALID_STATE;
    int16_t samples[PCM_SAMPLES];
    size_t count=(n-20)/2;
    for (size_t i=0;i<count;i++) {
        int16_t sample=(int16_t)((uint16_t)data[20+2*i]|((uint16_t)data[21+2*i]<<8));
        samples[i]=(int16_t)((int32_t)sample*gain/100);
    }
    esp_err_t error=bot_audio_playback_write(s_audio,playback,samples,count);
    if (error==ESP_OK) {
        portENTER_CRITICAL(&s_lock);
        if (s.playback_id==playback) {++s.playback_sequence;s.playback_received+=(uint32_t)(n-20);}
        portEXIT_CRITICAL(&s_lock);
    }
    return error;
}
/* A presentation is an output lease within an already authorized runtime turn.
 * It never opens microphone input and cannot revive an invalidated turn. */
static bool presentation_id_valid(const cJSON *item) {
    if (!cJSON_IsString(item) || !item->valuestring[0] || strlen(item->valuestring)>64) return false;
    for (const char *p=item->valuestring;*p;p++)
        if (!((*p>='a'&&*p<='z')||(*p>='A'&&*p<='Z')||(*p>='0'&&*p<='9')||
              *p=='-'||*p=='_'||*p=='.'||*p==':')) return false;
    return true;
}
static void presentation_segment(cJSON *root, bot_session_token_t token) {
    bot_session_token_t returned;
    cJSON *p=cJSON_GetObjectItemCaseSensitive(root,"payload");
    cJSON *pid=cJSON_GetObjectItemCaseSensitive(p,"presentation_id");
    cJSON *lease=cJSON_GetObjectItemCaseSensitive(p,"output_lease_id");
    cJSON *sid=cJSON_GetObjectItemCaseSensitive(p,"segment_id");
    cJSON *purpose=cJSON_GetObjectItemCaseSensitive(p,"purpose");
    cJSON *final=cJSON_GetObjectItemCaseSensitive(p,"final");
    cJSON *scene=cJSON_GetObjectItemCaseSensitive(p,"scene");
    cJSON *speech=cJSON_GetObjectItemCaseSensitive(scene,"speech");
    cJSON *expression=cJSON_GetObjectItemCaseSensitive(scene,"expression");
    cJSON *audio=cJSON_GetObjectItemCaseSensitive(p,"audio");
    uint32_t seq=0,hold=0,index=1;
    cJSON *index_json=cJSON_GetObjectItemCaseSensitive(p,"presentation_index");
    if (index_json && (!json_uint(index_json,64,&index) || !index)) return;
    if (!json_token(cJSON_GetObjectItemCaseSensitive(root,"runtime"),&returned) || !same(returned,token) ||
        !presentation_id_valid(pid) || !presentation_id_valid(lease) || !presentation_id_valid(sid) ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(p,"seq"),64,&seq) || !seq ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(p,"hold_ms"),30000,&hold) ||
        !cJSON_IsString(purpose) || !(cJSON_IsTrue(final)||cJSON_IsFalse(final)) ||
        !cJSON_IsObject(scene) || !cJSON_IsString(speech)) return;
    bot_playback_purpose_t playback_purpose;
    if (!strcmp(purpose->valuestring,"task_ack")) playback_purpose=BOT_PLAYBACK_TASK_ACK;
    else if (!strcmp(purpose->valuestring,"progress")) playback_purpose=BOT_PLAYBACK_PROGRESS;
    else if (!strcmp(purpose->valuestring,"content")) playback_purpose=BOT_PLAYBACK_CONTENT;
    else if (!strcmp(purpose->valuestring,"final")) playback_purpose=BOT_PLAYBACK_FINAL;
    else return;
    if (cJSON_IsTrue(final)!=(playback_purpose==BOT_PLAYBACK_FINAL)) return;
    cJSON *media=cJSON_GetObjectItemCaseSensitive(p,"media_kind");
    bool video=cJSON_IsString(media) && !strcmp(media->valuestring,"mjpeg_frame");
    if (media && !video) return;
    const char *asset_id="";
    cJSON *cards=cJSON_GetObjectItemCaseSensitive(scene,"cards"),*card;
    unsigned images=0;
    cJSON_ArrayForEach(card,cards) {
        cJSON *kind=cJSON_GetObjectItemCaseSensitive(card,"kind");
        if (!cJSON_IsString(kind)) return;
        if (!strcmp(kind->valuestring,"image")) {
            cJSON *asset=cJSON_GetObjectItemCaseSensitive(card,"asset");
            cJSON *id=cJSON_GetObjectItemCaseSensitive(asset,"id");
            if (++images>1 || !cJSON_IsString(id) || !asset_id_valid(id->valuestring)) return;
            asset_id=id->valuestring;
        } else if (strcmp(kind->valuestring,"text")) return;
    }
    /* Backpressure plus a minimum dwell bounds JPEG animations to <=5 fps.
     * No encoded video file or entire animation is buffered on this device. */
    if (video && !images) return;
    if (images && hold<200) hold=200;
    uint32_t stream=0,bytes=0,rate=0,channels=0,samples=0;
    bool has_audio=audio && !cJSON_IsNull(audio);
    cJSON *format=cJSON_GetObjectItemCaseSensitive(audio,"format");
    cJSON *ack=cJSON_GetObjectItemCaseSensitive(audio,"playback_ack");
    if (has_audio && (!cJSON_IsObject(audio) ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(audio,"stream_id"),UINT32_MAX,&stream) || !stream ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(audio,"bytes"),MAX_REPLY_PCM_BYTES,&bytes) || !bytes || bytes%2 ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(audio,"sample_rate"),16000,&rate) || rate!=16000 ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(audio,"channels"),1,&channels) || channels!=1 ||
        !json_uint(cJSON_GetObjectItemCaseSensitive(audio,"frame_samples"),PCM_SAMPLES,&samples) || samples!=PCM_SAMPLES ||
        !cJSON_IsString(format) || strcmp(format->valuestring,"pcm_s16le") ||
        !cJSON_IsString(ack) || strcmp(ack->valuestring,"hardware_v1"))) return;
    const int64_t now=esp_timer_get_time();
    portENTER_CRITICAL(&s_lock);
    bool valid=s.presentation_supported && s.authenticated && s.active && same(s.desired,token) &&
        same(s.remote,token) && s.phase==COMMITTED && !s.segment_active && !s.playback_id &&
        (!has_audio || s.session_spoken) &&
        s.presented_segments<64 &&
        ((!s.presentation_valid && seq==1 && index==1) ||
         (s.presentation_valid && index==s.presentation_index && seq==s.presentation_sequence+1 &&
          !strcmp(pid->valuestring,s.presentation_id) && !strcmp(lease->valuestring,s.output_lease_id) &&
          now-s.presentation_started_us<120000000) ||
         (s.presentation_valid && seq==1 && index==s.presentation_index+1 &&
          (s.playback_purpose==BOT_PLAYBACK_TASK_ACK || s.playback_purpose==BOT_PLAYBACK_PROGRESS) &&
          strcmp(pid->valuestring,s.presentation_id) && strcmp(lease->valuestring,s.output_lease_id)));
    if (valid) {
        if (!s.presentation_valid || index!=s.presentation_index) {
            s.presentation_started_us=now;s.video_started_us=0;
        }
        s.presentation_index=index;s.segment_generation=(index-1)*64+seq;++s.presented_segments;
        if (video && (!s.segment_video || !s.video_started_us)) s.video_started_us=now;
        s.segment_video=video;
        s.presentation_valid=true;s.segment_active=true;s.segment_final=cJSON_IsTrue(final);
        s.presentation_sequence=seq;s.segment_hold_ms=hold;s.segment_started_us=now;s.segment_rendered_us=0;
        s.segment_text_done=speech->valuestring[0]=='\0';s.segment_image_done=!images;s.segment_audio_done=!has_audio;
        s.segment_failed=false;s.segment_code[0]='\0';
        snprintf(s.presentation_id,sizeof(s.presentation_id),"%s",pid->valuestring);
        snprintf(s.output_lease_id,sizeof(s.output_lease_id),"%s",lease->valuestring);
        snprintf(s.segment_id,sizeof(s.segment_id),"%s",sid->valuestring);
        snprintf(s.segment_asset_id,sizeof(s.segment_asset_id),"%s",asset_id);
        s.result_token=token;s.result_valid=true;s.result_admitted=false;
        memset(&s_image_failure,0,sizeof(s_image_failure));
        s.playback_purpose=playback_purpose;
        s.phase=has_audio?PLAYBACK_PENDING:COMMITTED;
        if (has_audio) {
            s.playback_token=token;s.playback_stream=stream;s.playback_sequence=0;
            s.playback_expected=bytes;s.playback_received=0;s.playback_cancel_requested=false;
            s.playback_started_us=now;
        }
    }
    portEXIT_CRITICAL(&s_lock);
    if (!valid) return; /* stale, duplicate, out of order or backpressure violation */
    command(BOT_COMMAND_PRESENTATION_SEGMENT,token,speech->valuestring);
    if (cJSON_IsString(expression)) command(BOT_COMMAND_SET_EXPRESSION,token,expression->valuestring);
    queue_image_descriptor(scene,token);
}

static void text_message(const char *data,size_t n) {
    cJSON *root=cJSON_ParseWithLength(data,n); if (!root) { fail_session("Invalid secure response"); return; }
    cJSON *type=cJSON_GetObjectItemCaseSensitive(root,"type"),*id=cJSON_GetObjectItemCaseSensitive(root,"id"),
          *event=cJSON_GetObjectItemCaseSensitive(root,"event"),*ok=cJSON_GetObjectItemCaseSensitive(root,"ok");
    const char *kind=cJSON_IsString(type)?type->valuestring:"";
    if (!strcmp(kind,"welcome")) {
        cJSON *version=cJSON_GetObjectItemCaseSensitive(root,"v"),*dev=cJSON_GetObjectItemCaseSensitive(root,"device_id");
        if (cJSON_IsNumber(version) && version->valueint==3 && cJSON_IsString(dev) && !strcmp(dev->valuestring,CONFIG_ESPBOT_DEVICE_ID)) {
            bool supported=array_has_string(cJSON_GetObjectItemCaseSensitive(root,"capabilities"),"display.reply.v1");
            bool audio_supported=array_has_string(cJSON_GetObjectItemCaseSensitive(root,"capabilities"),"audio.reply.v1");
            portENTER_CRITICAL(&s_lock); s.authenticated=true; s.silent_supported=supported;
            s.audio_supported=audio_supported;
            s.presentation_supported=array_has_string(cJSON_GetObjectItemCaseSensitive(root,"capabilities"),"presentation.segments.v1");
            s.phase=READY; portEXIT_CRITICAL(&s_lock);
            link_event(true); ESP_LOGI(TAG,"Authenticated Mac gateway connected");
        } else fail_session("Gateway welcome invalid");
    } else if (!strcmp(kind,"response") && cJSON_IsString(id)) {
        char requested_asset[BOT_DISPLAY_ASSET_ID_BYTES]={0};
        portENTER_CRITICAL(&s_lock);
        phase_t phase=s.phase; bool opening=phase==OPENING&&!strcmp(id->valuestring,s.open_id);
        bool beginning=phase==BEGINNING&&!strcmp(id->valuestring,s.begin_id);
        bool completing=phase==COMMITTED&&!strcmp(id->valuestring,s.commit_id);
        bool image_reply=s.result_valid && *s.image_request_id && !strcmp(id->valuestring,s.image_request_id);
        bot_session_token_t current=s.remote; bool fresh=s.authenticated&&s.active&&same(s.desired,current);
        bot_session_token_t result_token=s.result_token;
        bool image_fresh=s.authenticated && s.result_valid;
        bool session_spoken=s.session_spoken;
        uint32_t begin_stream=s.stream;
        uint32_t image_sequence=s.presentation_valid?s.segment_generation:0;
        if (image_reply) {
            memcpy(requested_asset,s.image_asset_id,sizeof(requested_asset));
            s.image_request_id[0]='\0';
        }
        portEXIT_CRITICAL(&s_lock);
        if (image_fresh && image_reply) {
            current=result_token;
            if (!cJSON_IsTrue(ok)) {
                cJSON *error=cJSON_GetObjectItemCaseSensitive(root,"error");
                cJSON *code=cJSON_GetObjectItemCaseSensitive(error,"code");
                brain_secure_image_failed(current,requested_asset,
                    cJSON_IsString(code)?code->valuestring:"image_request_failed");
                goto done;
            }
            cJSON *payload=cJSON_GetObjectItemCaseSensitive(root,"payload");
            cJSON *asset=cJSON_GetObjectItemCaseSensitive(payload,"asset_id");
            cJSON *data=cJSON_GetObjectItemCaseSensitive(payload,"data_base64");
            cJSON *eof=cJSON_GetObjectItemCaseSensitive(payload,"eof");
            bot_session_token_t returned;
            uint32_t offset,bytes;
            bool valid=json_token(cJSON_GetObjectItemCaseSensitive(root,"runtime"),&returned) &&
                same(returned,current) && cJSON_IsString(asset) && !strcmp(asset->valuestring,requested_asset) &&
                cJSON_IsString(data) && strlen(data->valuestring)<=4096 && (cJSON_IsTrue(eof)||cJSON_IsFalse(eof)) &&
                json_uint(cJSON_GetObjectItemCaseSensitive(payload,"offset"),65535,&offset) &&
                json_uint(cJSON_GetObjectItemCaseSensitive(payload,"bytes"),3072,&bytes) && bytes;
            if (!valid) {
                brain_secure_image_failed(current,requested_asset,"image_chunk_invalid");
                goto done;
            }
            image_message_t *message=heap_caps_calloc(1,sizeof(*message),MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
            if (!message) {
                brain_secure_image_failed(current,requested_asset,"image_no_memory");
                goto done;
            }
            message->kind=IMAGE_CHUNK;message->token=current;
            message->presentation_sequence=image_sequence;
            message->offset=offset;message->bytes=bytes;message->eof=cJSON_IsTrue(eof);
            snprintf(message->asset_id,sizeof(message->asset_id),"%s",requested_asset);
            snprintf(message->base64,sizeof(message->base64),"%s",data->valuestring);
            queue_image_message(message);
        } else if (fresh && (opening||beginning||completing) && !cJSON_IsTrue(ok)) {
            finish_with_message(current,request_error(root),completing);
        }
        else if (fresh && cJSON_IsTrue(ok) && opening) {
            cJSON *payload=cJSON_GetObjectItemCaseSensitive(root,"payload");
            cJSON *output=cJSON_GetObjectItemCaseSensitive(payload,"output");
            cJSON *version=cJSON_GetObjectItemCaseSensitive(output,"version");
            cJSON *audio=cJSON_GetObjectItemCaseSensitive(output,"audio");
            bot_session_token_t returned;
            if (!json_token(cJSON_GetObjectItemCaseSensitive(payload,"runtime"),&returned) || !same(returned,current) ||
                !cJSON_IsNumber(version) || version->valuedouble!=1 ||
                !(session_spoken?cJSON_IsTrue(audio):cJSON_IsFalse(audio)) ||
                !array_has_string(cJSON_GetObjectItemCaseSensitive(output,"cards"),"text")) {
                finish_with_message(current,"请更新 Mac 小呆服务",false);
            } else {
                portENTER_CRITICAL(&s_lock); if (s.phase==OPENING) s.phase=OPEN; portEXIT_CRITICAL(&s_lock);
            }
        } else if (fresh && cJSON_IsTrue(ok) && beginning) {
            cJSON *payload=cJSON_GetObjectItemCaseSensitive(root,"payload");
            cJSON *plain=cJSON_GetObjectItemCaseSensitive(payload,"plain_audio");
            cJSON *host=cJSON_GetObjectItemCaseSensitive(plain,"host");
            cJSON *port=cJSON_GetObjectItemCaseSensitive(plain,"port");
            cJSON *key=cJSON_GetObjectItemCaseSensitive(plain,"key");
            cJSON *runtime=cJSON_GetObjectItemCaseSensitive(payload,"runtime");
            uint8_t decoded[32]={0};
            uint32_t returned_stream=0;bot_session_token_t returned;
            struct in_addr address;
            bool valid=json_uint(cJSON_GetObjectItemCaseSensitive(payload,"stream_id"),UINT32_MAX,&returned_stream) &&
                returned_stream==begin_stream && (!runtime || (json_token(runtime,&returned) && same(returned,current))) &&
                cJSON_IsString(host) && cJSON_IsNumber(port) && cJSON_IsString(key) &&
                strlen(host->valuestring)<sizeof(s.plain_host) &&
                inet_pton(AF_INET,host->valuestring,&address)==1 &&
                port->valuedouble>=1 && port->valuedouble<=65535 &&
                port->valuedouble==(double)(uint16_t)port->valuedouble &&
                decode_key(key->valuestring,decoded);
            if (valid) {
                portENTER_CRITICAL(&s_lock);
                if (s.phase==BEGINNING) {
                    snprintf(s.plain_host,sizeof(s.plain_host),"%s",host->valuestring);
                    s.plain_port=(uint16_t)port->valuedouble;
                    memcpy(s.plain_key,decoded,sizeof(decoded));
                    s.phase=PLAIN_CONNECTING;
                }
                portEXIT_CRITICAL(&s_lock);
            } else fail_operation(current,"Plain audio negotiation failed");
            memset(decoded,0,sizeof(decoded));
            if (cJSON_IsString(key) && key->valuestring) memset(key->valuestring,0,strlen(key->valuestring));
        } else if (fresh && cJSON_IsTrue(ok) && completing) {
            bot_session_token_t returned;
            if (!json_token(cJSON_GetObjectItemCaseSensitive(root,"runtime"),&returned) || !same(returned,current)) goto done;
            cJSON *payload=cJSON_GetObjectItemCaseSensitive(root,"payload");
            if (cJSON_IsTrue(cJSON_GetObjectItemCaseSensitive(payload,"presentation_pending"))) goto done;
            cJSON *scene=cJSON_GetObjectItemCaseSensitive(payload,"scene"),
                  *speech=cJSON_GetObjectItemCaseSensitive(scene,"speech"),*expression=cJSON_GetObjectItemCaseSensitive(scene,"expression"),
                  *audio=cJSON_GetObjectItemCaseSensitive(root,"audio");
            uint32_t stream=0,bytes=0,rate=0,channels=0,samples=0;
            bool has_audio=audio && !cJSON_IsNull(audio);
            cJSON *format=cJSON_GetObjectItemCaseSensitive(audio,"format");
            cJSON *ack=cJSON_GetObjectItemCaseSensitive(audio,"playback_ack");
            bool valid_audio=cJSON_IsObject(audio) && session_spoken &&
                json_uint(cJSON_GetObjectItemCaseSensitive(audio,"stream_id"),UINT32_MAX,&stream) && stream &&
                json_uint(cJSON_GetObjectItemCaseSensitive(audio,"bytes"),MAX_REPLY_PCM_BYTES,&bytes) && bytes && !(bytes%2) &&
                json_uint(cJSON_GetObjectItemCaseSensitive(audio,"sample_rate"),16000,&rate) && rate==16000 &&
                json_uint(cJSON_GetObjectItemCaseSensitive(audio,"channels"),1,&channels) && channels==1 &&
                json_uint(cJSON_GetObjectItemCaseSensitive(audio,"frame_samples"),PCM_SAMPLES,&samples) && samples==PCM_SAMPLES &&
                cJSON_IsString(format) && !strcmp(format->valuestring,"pcm_s16le") &&
                cJSON_IsString(ack) && !strcmp(ack->valuestring,"hardware_v1");
            if (has_audio && !valid_audio) {
                portENTER_CRITICAL(&s_lock);s.cancel_before_next=true;portEXIT_CRITICAL(&s_lock);
                finish_with_message(current,"Mac 回复格式有误，请更新服务",true);
                goto done;
            }
            portENTER_CRITICAL(&s_lock);
            s.presentation_valid=false;s.segment_active=false;
            s.playback_purpose=BOT_PLAYBACK_FINAL;
            s.result_token=current;s.result_valid=true;s.result_admitted=false;
            s.phase=valid_audio?PLAYBACK_PENDING:WAIT_NEXT;
            if (valid_audio) {
                s.playback_token=current;s.playback_stream=stream;s.playback_sequence=0;
                s.playback_expected=bytes;s.playback_received=0;s.playback_cancel_requested=false;
                s.playback_started_us=esp_timer_get_time();
            }
            portEXIT_CRITICAL(&s_lock);
            if (cJSON_IsString(expression)) command(BOT_COMMAND_SET_EXPRESSION,current,expression->valuestring);
            command(valid_audio?BOT_COMMAND_REPLY_PRESENT:BOT_COMMAND_REPLY_DONE,current,
                cJSON_IsString(speech)?speech->valuestring:NULL);
            queue_image_descriptor(scene,current);
        }
    } else if (!strcmp(kind,"event") && cJSON_IsString(event)) {
        bot_session_token_t token;
        portENTER_CRITICAL(&s_lock); bool fresh=s.authenticated&&s.active&&same(s.desired,s.remote); token=s.remote; phase_t phase=s.phase; portEXIT_CRITICAL(&s_lock);
        if (!strcmp(event->valuestring,"presentation.segment") && fresh) {
            presentation_segment(root,token);
        } else if (!strcmp(event->valuestring,"turn.accepted") && fresh && phase==COMMITTED) {
            bot_session_token_t returned;
            if (json_token(cJSON_GetObjectItemCaseSensitive(root,"runtime"),&returned) && same(returned,token)) {
                command(BOT_COMMAND_THINKING,token,NULL);
                command(BOT_COMMAND_SET_CAPTION,token,"正在处理");
            }
        } else if (!strcmp(event->valuestring,"turn.progress") && fresh && phase==COMMITTED && cJSON_IsString(id)) {
            bot_session_token_t returned;
            cJSON *payload=cJSON_GetObjectItemCaseSensitive(root,"payload");
            cJSON *seq=cJSON_GetObjectItemCaseSensitive(payload,"seq");
            cJSON *stage=cJSON_GetObjectItemCaseSensitive(payload,"stage");
            cJSON *preview=cJSON_GetObjectItemCaseSensitive(payload,"preview");
            if (!json_token(cJSON_GetObjectItemCaseSensitive(root,"runtime"),&returned) || !same(returned,token) ||
                !cJSON_IsNumber(seq) || seq->valuedouble<1 || seq->valuedouble>4294967295.0 ||
                seq->valuedouble!=(double)(uint32_t)seq->valuedouble || !cJSON_IsString(stage)) goto done;
            const char *message=NULL;
            bool approval=false;
            if (!strcmp(stage->valuestring,"transcribing")) message="正在听懂";
            else if (!strcmp(stage->valuestring,"understood")) message=cJSON_IsString(preview)?preview->valuestring:"听懂了";
            else if (!strcmp(stage->valuestring,"working")) message="正在处理";
            else if (!strcmp(stage->valuestring,"awaiting_parent")) {message="请在 Mac 上确认";approval=true;}
            else if (!strcmp(stage->valuestring,"preparing_image")) message="正在准备图片";
            else if (!strcmp(stage->valuestring,"synthesizing")) message="正在准备语音";
            if (!message) goto done;
            portENTER_CRITICAL(&s_lock);
            bool valid=s.phase==COMMITTED && s.authenticated && s.active && same(s.desired,returned) &&
                same(s.remote,returned) && !strcmp(id->valuestring,s.commit_id) &&
                (uint32_t)seq->valuedouble>s.progress_seq &&
                esp_timer_get_time()-s.commit_started_us<180000000;
            if (valid) s.progress_seq=(uint32_t)seq->valuedouble;
            portEXIT_CRITICAL(&s_lock);
            if (valid) {
                progress_command(token,approval);
                command(BOT_COMMAND_SET_CAPTION,token,message);
            }
        } else if (!strcmp(event->valuestring,"audio.complete") && fresh && phase==PLAYING) {
            /* TCP and WSS are independent; the authenticated plain-audio end
             * frame, not this control event, closes local playback. */
        } else if (!strcmp(event->valuestring,"audio.cancelled")) {
            bot_session_token_t returned;uint32_t stream;
            portENTER_CRITICAL(&s_lock);
            bool current=json_token(cJSON_GetObjectItemCaseSensitive(root,"runtime"),&returned) &&
                json_uint(cJSON_GetObjectItemCaseSensitive(root,"stream_id"),UINT32_MAX,&stream) &&
                s.playback_stream==stream && same(returned,s.playback_token) && same(returned,s.desired);
            const uint32_t playback=s.playback_id;
            portEXIT_CRITICAL(&s_lock);
            // Whole-session cancellation owns its reason independently. A
            // companion stream event must not replace that terminal notice.
            if (current) {
                cJSON *session=cJSON_GetObjectItemCaseSensitive(root,"session_cancelled");
                if (!cJSON_IsTrue(session)) fail_operation(returned,"Server cancelled the current playback");
                else if (s_audio && playback) bot_audio_playback_cancel(s_audio,playback);
            }
        } else if (!strcmp(event->valuestring,"session.cancelled") || !strcmp(event->valuestring,"session.expired")) {
            bot_session_token_t returned;
            cJSON *runtime=cJSON_GetObjectItemCaseSensitive(root,"runtime");
            /* Older servers omit the token; their whole authenticated session
             * cancellation remains supported, never an old audio stream event. */
            if (fresh && (!runtime || (json_token(runtime,&returned) && same(returned,token)))) {
                cJSON *code=cJSON_GetObjectItemCaseSensitive(root,"code");
                bot_session_end_code_t why=BOT_SESSION_END_SERVICE_STOPPED;
                if (!strcmp(event->valuestring,"session.expired")) why=BOT_SESSION_END_EXPIRED;
                else if (cJSON_IsString(code) && !strcmp(code->valuestring,"parent_stop")) why=BOT_SESSION_END_PARENT_STOP;
                else if (cJSON_IsString(code) && (!strcmp(code->valuestring,"memory_changed") ||
                         !strcmp(code->valuestring,"identity_revoked"))) why=BOT_SESSION_END_MEMORY_CHANGED;
                fail_session_token("Server cancelled or expired the session",&token,why);
            }
        }
    } else if (!strcmp(kind,"error")) {
        cJSON *runtime=cJSON_GetObjectItemCaseSensitive(root,"runtime");
        bot_session_token_t returned;
        portENTER_CRITICAL(&s_lock);
        returned=s.remote;
        bool current=s.authenticated && s.active && same(s.desired,s.remote) &&
            (!runtime || (json_token(runtime,&returned) && same(returned,s.remote)));
        portEXIT_CRITICAL(&s_lock);
        if (current) fail_operation(returned,"Gateway rejected current audio");
    }
done:
    cJSON_Delete(root);
}
static bool load_tls_identity(void) {
    struct http_parser_url url;
    http_parser_url_init(&url);
    if (http_parser_parse_url(s_provision.url,strlen(s_provision.url),0,&url) ||
        !url.field_data[UF_HOST].len || url.field_data[UF_HOST].len >= sizeof(s_tls_name)) return false;
    snprintf(s_tls_name,sizeof(s_tls_name),"%.*s",(int)url.field_data[UF_HOST].len,
             s_provision.url+url.field_data[UF_HOST].off);
    const char *body = strchr(s_provision.cert,'\n'), *end = strstr(s_provision.cert,"-----END CERTIFICATE-----");
    if (!body || !end || ++body >= end) return false;
    size_t n=0; unsigned char hash[32];
    unsigned char *der = heap_caps_malloc(CERT_SIZE,MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
    if (!der) return false;
    bool ok = mbedtls_base64_decode(der,CERT_SIZE,&n,(const unsigned char *)body,end-body)==0 && n &&
              mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),der,n,hash)==0;
    heap_caps_free(der);
    if (ok) for (size_t i=0;i<sizeof(hash);++i) snprintf(s_ca_fingerprint+2*i,3,"%02x",hash[i]);
    return ok;
}

static void refresh_gateway(esp_websocket_client_handle_t client) {
    char ip[16], uri[96]; uint16_t port;
    /* BEFORE_CONNECT runs synchronously on the client's own task, before its
     * transport opens a socket. Wait here for the paired service on every dial.
     * The local runtime and Wi-Fi event loop continue independently. */
    while (atomic_load(&s_discovery_enabled)) {
        esp_err_t found=bot_lan_find_secure_gateway(s_ca_fingerprint,ip,&port);
        if (found!=ESP_OK) { vTaskDelay(pdMS_TO_TICKS(900)); continue; }
        snprintf(uri,sizeof(uri),"wss://%s:%u/v3/brain/ws",ip,port);
        if (strcmp(s_dial_uri,uri)) {
            if (esp_websocket_client_set_uri(client,uri)!=ESP_OK) { vTaskDelay(pdMS_TO_TICKS(900)); continue; }
            snprintf(s_dial_uri,sizeof(s_dial_uri),"%s",uri);
            ESP_LOGI(TAG,"Bonjour v3 selected paired Mac %s:%u",ip,port);
            if (strcmp(s_peer_ip,ip)) { s_switch_path=false; s_via_gateway=false; }
            snprintf(s_peer_ip,sizeof(s_peer_ip),"%s",ip);
        }
        break;
    }
    if (!atomic_load(&s_discovery_enabled)) return;
    if (s_peer_ip[0]) {
        if (s_switch_path) s_via_gateway=!s_via_gateway;
        if (!brain_peer_path(s_peer_ip,s_via_gateway)) s_via_gateway=false;
        if (s_switch_path) ESP_LOGI(TAG,"Retrying paired Mac %s via %s",s_peer_ip,s_via_gateway?"DHCP gateway":"direct LAN");
    }
    s_switch_path=false; s_connect_pending=true; s_connect_us=esp_timer_get_time();
}

static void ws_event(void *arg,esp_event_base_t base,int32_t id,void *event_data) {
    (void)arg;(void)base;
    esp_websocket_event_data_t *e=event_data;
    if (id==WEBSOCKET_EVENT_BEFORE_CONNECT && e) {
        refresh_gateway(e->client);
    } else if (id==WEBSOCKET_EVENT_CONNECTED) {
        s_connect_pending=false;
        s_rx_used=0; portENTER_CRITICAL(&s_lock);s.phase=WELCOME;s.authenticated=false;s.audio_allowed=false;portEXIT_CRITICAL(&s_lock);
    } else if (id==WEBSOCKET_EVENT_DISCONNECTED || id==WEBSOCKET_EVENT_ERROR ||
               id==WEBSOCKET_EVENT_CLOSED) {
        /* The official client reports a clean server CLOSE separately from
         * transport loss. Revoke the authenticated/audio leases in both cases
         * before auto-reconnect; otherwise a new local wake tries a dead Mac
         * session and gets cancelled after its acknowledgement. */
        if (id==WEBSOCKET_EVENT_DISCONNECTED && e && s_connect_pending &&
            e->error_handle.error_type==WEBSOCKET_ERROR_TYPE_TCP_TRANSPORT &&
            e->error_handle.esp_ws_handshake_status_code==0 && e->error_handle.esp_tls_cert_verify_flags==0 &&
            (e->error_handle.esp_tls_last_esp_err==ESP_ERR_ESP_TLS_CONNECTION_TIMEOUT ||
             e->error_handle.esp_transport_sock_errno==ETIMEDOUT ||
             (e->error_handle.esp_tls_stack_err==0 && esp_timer_get_time()-s_connect_us>=9500000))) {
            s_switch_path=true;
        }
        brain_secure_network_down();
    }
    else if (id==WEBSOCKET_EVENT_DATA && e && (e->op_code==1 || e->op_code==2 || e->op_code==0)) {
        if (e->payload_offset==0) {s_rx_used=0;s_rx_opcode=e->op_code;}
        if (e->payload_len<0 || e->payload_len>RX_SIZE || e->payload_offset!=(int)s_rx_used ||
            e->data_len<0 || s_rx_used+(size_t)e->data_len>RX_SIZE) {s_rx_used=0;fail_session("Secure message too large or fragmented incorrectly");return;}
        memcpy(s_rx+s_rx_used,e->data_ptr,e->data_len);s_rx_used+=(size_t)e->data_len;
        if (s_rx_used==(size_t)e->payload_len) {
            if (s_rx_opcode==1) {s_rx[s_rx_used]=0;text_message(s_rx,s_rx_used);}
            else if (s_rx_opcode==2) fail_session("Audio used an unnegotiated transport");
            memset(s_rx,0,s_rx_used);s_rx_used=0;
        }
    }
}

static void flush_pcm(void) {
    pcm_frame_t packet;
    while (s_pcm && xQueueReceive(s_pcm,&packet,0)==pdTRUE) memset(&packet,0,sizeof(packet));
}
static bool send_text(const char *text) {
    size_t len=strlen(text);
    return s_ws && esp_websocket_client_is_connected(s_ws) &&
        esp_websocket_client_send_text(s_ws,text,(int)len,pdMS_TO_TICKS(1000))==(int)len;
}
static void plain_close(void) {
    if (s_plain_fd>=0) close(s_plain_fd);
    s_plain_fd=-1;s_plain_used=0;s_plain_blocked_us=0;
    memset(s_plain_key,0,sizeof(s_plain_key));
    if (s_plain_rx) memset(s_plain_rx,0,PLAIN_RX_SIZE);
}
static bool plain_connect(const secure_state_t *state) {
    plain_close();
    int fd=socket(AF_INET,SOCK_STREAM,IPPROTO_TCP);
    if (fd<0) return false;
    int flags=fcntl(fd,F_GETFL,0);
    if (flags<0 || fcntl(fd,F_SETFL,flags|O_NONBLOCK)<0) {close(fd);return false;}
    struct sockaddr_in addr={.sin_family=AF_INET,.sin_port=htons(state->plain_port)};
    if (inet_pton(AF_INET,state->plain_host,&addr.sin_addr)!=1) {close(fd);return false;}
    if (connect(fd,(struct sockaddr *)&addr,sizeof(addr))<0 && errno!=EINPROGRESS) {close(fd);return false;}
    fd_set writable;FD_ZERO(&writable);FD_SET(fd,&writable);
    struct timeval wait={.tv_sec=3,.tv_usec=0};
    int ready=select(fd+1,NULL,&writable,NULL,&wait), error=0;socklen_t size=sizeof(error);
    if (ready!=1 || getsockopt(fd,SOL_SOCKET,SO_ERROR,&error,&size)<0 || error!=0) {close(fd);return false;}
    s_plain_fd=fd;memcpy(s_plain_key,state->plain_key,sizeof(s_plain_key));
    return true;
}
static bool plain_hmac(const unsigned char *data,size_t length,unsigned char out[PLAIN_MAC_SIZE]) {
    const mbedtls_md_info_t *md=mbedtls_md_info_from_type(MBEDTLS_MD_SHA256);
    return md && mbedtls_md_hmac(md,s_plain_key,sizeof(s_plain_key),data,length,out)==0;
}
static bool plain_send_all(const unsigned char *data,size_t length,bot_session_token_t token) {
    size_t sent=0;int64_t deadline=esp_timer_get_time()+1500000;
    int last_error=0;unsigned attempts=0;
    while (sent<length && esp_timer_get_time()<deadline) {
        portENTER_CRITICAL(&s_lock);
        bool valid=s.authenticated && s.active && same(s.desired,token) && same(s.remote,token);
        portEXIT_CRITICAL(&s_lock);
        if (!valid || s_plain_fd<0) {
            ESP_LOGW(TAG,"Plain audio send stopped: session changed or socket closed");
            return false;
        }
        int n=send(s_plain_fd,data+sent,length-sent,MSG_DONTWAIT);
        ++attempts;
        if (n>0) {sent+=(size_t)n;continue;}
        last_error=n<0?errno:0;
        if (n<0 && errno!=EAGAIN && errno!=EWOULDBLOCK && errno!=EINTR) {
            ESP_LOGW(TAG,"Plain audio socket write failed: errno=%d",last_error);
            return false;
        }
        fd_set writable;FD_ZERO(&writable);FD_SET(s_plain_fd,&writable);
        struct timeval wait={.tv_sec=0,.tv_usec=50000};
        if (select(s_plain_fd+1,NULL,&writable,NULL,&wait)<0 && errno!=EINTR) {
            ESP_LOGW(TAG,"Plain audio socket select failed: errno=%d",errno);
            return false;
        }
    }
    if (sent!=length) ESP_LOGW(TAG,"Plain audio write timed out: sent=%u/%u attempts=%u errno=%d internal_free=%u largest=%u",
        (unsigned)sent,(unsigned)length,attempts,last_error,
        (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL|MALLOC_CAP_8BIT),
        (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL|MALLOC_CAP_8BIT));
    return sent==length;
}
static bool plain_receive(void) {
    if (s_plain_fd<0 || !s_plain_rx) return false;
    /* Read only after parsing the buffered frame. Backpressure must never
     * consume a sequence number or drop bytes from the authenticated stream. */
    for (unsigned packets=0;packets<8;) {
        if (s_plain_used>=4) {
            size_t length=((size_t)s_plain_rx[0]<<24)|((size_t)s_plain_rx[1]<<16)|
                ((size_t)s_plain_rx[2]<<8)|s_plain_rx[3];
            if (length<1+PLAIN_MAC_SIZE || length>2048) return false;
            if (s_plain_used>=4+length) {
                unsigned char *body=s_plain_rx+4;
                size_t content=length-PLAIN_MAC_SIZE;
                unsigned char tag[PLAIN_MAC_SIZE];
                if (!plain_hmac(body,content,tag)) return false;
                unsigned char mismatch=0;
                for (size_t i=0;i<sizeof(tag);i++) mismatch|=tag[i]^body[content+i];
                memset(tag,0,sizeof(tag));
                if (mismatch) return false;
                esp_err_t accepted=ESP_ERR_INVALID_ARG;
                bool ending=body[0]=='E' && content==9;
                if (body[0]=='O') accepted=playback_frame(body+1,content-1);
                else if (ending) {
                    uint32_t stream=read32(body+1),frames=read32(body+5),playback;
                    portENTER_CRITICAL(&s_lock);
                    bool valid=s.authenticated && s.active && s.phase==PLAYING &&
                        same(s.desired,s.remote) && stream==s.playback_stream &&
                        frames==s.playback_sequence && s.playback_received==s.playback_expected;
                    playback=s.playback_id;
                    portEXIT_CRITICAL(&s_lock);
                    if (!valid || !playback || s_plain_used!=4+length) return false;
                    accepted=bot_audio_playback_end(s_audio,playback);
                    if (accepted==ESP_OK) {
                        portENTER_CRITICAL(&s_lock);
                        if (s.playback_id==playback && s.phase==PLAYING) s.phase=DRAINING;
                        portEXIT_CRITICAL(&s_lock);
                    }
                }
                if (accepted==ESP_ERR_TIMEOUT) {
                    if (!s_plain_blocked_us) s_plain_blocked_us=esp_timer_get_time();
                    return esp_timer_get_time()-s_plain_blocked_us<2000000;
                }
                if (accepted!=ESP_OK) return false;
                s_plain_blocked_us=0;
                size_t consumed=4+length;
                memmove(s_plain_rx,s_plain_rx+consumed,s_plain_used-consumed);
                s_plain_used-=consumed;++packets;
                if (ending) return true; /* hardware DONE, not E, finishes the turn */
                continue;
            }
        }
        if (s_plain_used>=PLAIN_RX_SIZE) return false;
        int n=recv(s_plain_fd,s_plain_rx+s_plain_used,PLAIN_RX_SIZE-s_plain_used,MSG_DONTWAIT);
        if (n==0) return false;
        if (n<0) return errno==EAGAIN || errno==EWOULDBLOCK || errno==EINTR;
        s_plain_used+=(size_t)n;
    }
    return true;
}
static void runtime_json(char *out,size_t capacity,bot_session_token_t t) {
    snprintf(out,capacity,"{\"epoch\":%lu,\"session_id\":%lu,\"turn_id\":%lu,\"focus_generation\":%lu}",
        (unsigned long)t.epoch,(unsigned long)t.session_id,(unsigned long)t.turn_id,(unsigned long)t.focus_generation);
}
static void request_id(char *out,size_t capacity) { snprintf(out,capacity,"r%lu",(unsigned long)++s_request); }
static bool simple_request(const char *method) {
    char msg[180], id[24]; request_id(id,sizeof(id));
    snprintf(msg,sizeof(msg),"{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"%s\",\"payload\":{}}",id,method);
    return send_text(msg);
}
static bool playback_report(const playback_event_t *event, const char *status, const char *code) {
    char id[24],runtime[180],message[480];
    request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),event->token);
    int n=snprintf(message,sizeof(message),
        "{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"audio.playback.report\",\"payload\":{\"runtime\":%s,\"stream_id\":%lu,\"status\":\"%s\",\"frames\":%lu,\"bytes\":%lu,\"code\":\"%s\"}}",
        id,runtime,(unsigned long)event->stream,status,(unsigned long)event->frames,
        (unsigned long)event->bytes,code?code:"");
    return n>0 && (size_t)n<sizeof(message) && send_text(message);
}
static bool playback_event_poll(void) {
    playback_event_t event;
    while (s_playback_events && xQueuePeek(s_playback_events,&event,0)==pdTRUE) {
        portENTER_CRITICAL(&s_lock);
        bool current=event.playback_id==s.playback_id && same(event.token,s.playback_token);
        bool managed=event.presentation_sequence!=0;
        bool authenticated=s.authenticated;
        bool live=s.active && same(event.token,s.desired);
        portEXIT_CRITICAL(&s_lock);
        if (!current) {(void)xQueueReceive(s_playback_events,&event,0);continue;}
        const char *status=event.kind==BOT_AUDIO_EVENT_PLAYBACK_STARTED?"started":
            event.kind==BOT_AUDIO_EVENT_PLAYBACK_DONE?"finished":
            event.kind==BOT_AUDIO_EVENT_PLAYBACK_CANCELLED?"cancelled":"failed";
        const char *code=event.kind==BOT_AUDIO_EVENT_PLAYBACK_FAILED?
            (event.playback_id?"hardware_playback_failed":"playback_unavailable"):"";
        if (authenticated && !playback_report(&event,status,code)) {
            /* Keep both receipt and stream lease. Starting the next capture
             * here would race a Mac still waiting for this hardware terminal. */
            ESP_LOGW(TAG,"Playback receipt pending; next transport waits for delivery");
            return false;
        }
        (void)xQueueReceive(s_playback_events,&event,0);
        ESP_LOGI(TAG,"Reply audio %s: stream=%lu frames=%lu bytes=%lu",status,
            (unsigned long)event.stream,(unsigned long)event.frames,(unsigned long)event.bytes);
        if (event.kind!=BOT_AUDIO_EVENT_PLAYBACK_STARTED) {
            portENTER_CRITICAL(&s_lock);
            if (s.playback_id==event.playback_id) {
                s.playback_id=0;s.playback_cancel_requested=false;
                if (s.phase==PLAYING || s.phase==DRAINING || s.phase==PLAYBACK_PENDING)
                    s.phase=managed?COMMITTED:WAIT_NEXT;
                if (managed && s.presentation_valid && event.presentation_sequence==s.segment_generation) {
                    s.segment_audio_done=true;
                    if (event.kind!=BOT_AUDIO_EVENT_PLAYBACK_DONE) {
                        s.segment_failed=true;
                        snprintf(s.segment_code,sizeof(s.segment_code),"%s",*code?code:"playback_cancelled");
                    }
                }
            }
            portEXIT_CRITICAL(&s_lock);
            /* A first codec write can fail before STARTED. Runtime cannot infer
             * a hardware completion in that case; preserve the presented text
             * and explicitly finish its text-only fallback. A later/stale
             * command cannot advance a newer turn. */
            if (live && authenticated && event.kind!=BOT_AUDIO_EVENT_PLAYBACK_DONE) {
                command(BOT_COMMAND_FAILURE_CAPTION,event.token,"语音未播放，文字仍可阅读");
                if (!managed) command(BOT_COMMAND_REPLY_DONE,event.token,NULL);
            }
        }
    }
    return true;
}
static void playback_prepare(const secure_state_t *state) {
    /* Wait for the segment's display outcome; narration starts only after a
     * successful flush, never while its illustration is downloading. */
    if (state->presentation_valid && state->segment_active &&
        (!state->segment_text_done || !state->segment_image_done)) return;
    if (state->presentation_valid && state->segment_active && state->segment_failed) {
        /* An actual download/decode/display failure invalidates the frozen
         * segment too. Do not play narration referring to its missing image;
         * report the unstarted audio failure before the aggregate receipt. */
        const playback_event_t failed={.kind=BOT_AUDIO_EVENT_PLAYBACK_FAILED,
            .token=state->remote,.stream=state->playback_stream,.error=ESP_ERR_INVALID_STATE,
            .presentation_sequence=state->segment_generation};
        if (xQueueSend(s_playback_events,&failed,0)!=pdTRUE)
            fail_operation(state->remote,"Playback failure receipt queue full");
        return;
    }
    const bot_audio_token_t token={state->remote.epoch,state->remote.session_id,
        state->remote.turn_id,state->remote.focus_generation};
    uint32_t playback=0;
    esp_err_t error=state->presentation_valid && state->segment_active ?
        bot_audio_playback_begin_segment(s_audio,&token,state->playback_purpose,&playback) :
        bot_audio_playback_begin(s_audio,&token,&playback);
    if (error!=ESP_OK) {
        int64_t ready_since=(state->presentation_valid && state->segment_active && state->segment_rendered_us)?
            state->segment_rendered_us:state->playback_started_us;
        if (esp_timer_get_time()-ready_since<1500000 &&
            (error==ESP_ERR_TIMEOUT || error==ESP_ERR_INVALID_STATE)) return;
        const playback_event_t failed={.kind=BOT_AUDIO_EVENT_PLAYBACK_FAILED,
            .token=state->remote,.stream=state->playback_stream,.error=error,
            .presentation_sequence=state->presentation_valid?state->segment_generation:0};
        if (xQueueSend(s_playback_events,&failed,0)!=pdTRUE)
            fail_operation(state->remote,"Playback failure receipt queue full");
        return;
    }
    portENTER_CRITICAL(&s_lock);
    bool valid=s.authenticated && s.active && s.phase==PLAYBACK_PENDING &&
        same(s.desired,state->remote) && same(s.remote,state->remote);
    /* Retain the exact lease even when cancellation raced BEGIN, so its
     * terminal event can be reported without ever cancelling a newer clip. */
    s.playback_id=playback;s.playback_token=state->remote;
    if (valid) {s.phase=PLAYING;s.playback_gain_percent=s.reply_gain_percent;s.playback_started_us=esp_timer_get_time();}
    else s.playback_cancel_requested=true;
    portEXIT_CRITICAL(&s_lock);
    if (!valid) {bot_audio_playback_cancel(s_audio,playback);return;}
    char id[24],runtime[180],message[380];
    request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),state->remote);
    snprintf(message,sizeof(message),
        "{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"audio.playback.ready\",\"payload\":{\"runtime\":%s,\"stream_id\":%lu}}",
        id,runtime,(unsigned long)state->playback_stream);
    if (!send_text(message)) fail_operation(state->remote,"Could not acknowledge playback readiness");
    else ESP_LOGI(TAG,"Reply audio ready: stream=%lu bytes=%lu playback=%lu",
        (unsigned long)state->playback_stream,(unsigned long)state->playback_expected,(unsigned long)playback);
}
typedef struct {
    bot_session_token_t token;
    uint8_t *jpeg;
    uint32_t total, offset;
    uint32_t presentation_sequence;
    uint16_t width, height;
    char id[BOT_DISPLAY_ASSET_ID_BYTES], digest[65];
    int64_t requested_us;
    unsigned retries;
} image_download_t;

static void image_discard(image_download_t *download) {
    free(download->jpeg);
    memset(download,0,sizeof(*download));
    portENTER_CRITICAL(&s_lock);
    s.image_request_id[0]='\0';s.image_asset_id[0]='\0';
    portEXIT_CRITICAL(&s_lock);
}
static bool image_request(image_download_t *download) {
    char id[24], runtime[180], message[440];
    request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),download->token);
    int n=snprintf(message,sizeof(message),
        "{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"display.asset.read\",\"payload\":{\"runtime\":%s,\"asset_id\":\"%s\",\"offset\":%lu,\"length\":3072}}",
        id,runtime,download->id,(unsigned long)download->offset);
    if (n<0 || n>=sizeof(message)) return false;
    portENTER_CRITICAL(&s_lock);
    if (!s.authenticated || !s.result_valid || !same(download->token,s.result_token)) {
        portEXIT_CRITICAL(&s_lock);
        return false;
    }
    snprintf(s.image_request_id,sizeof(s.image_request_id),"%s",id);
    snprintf(s.image_asset_id,sizeof(s.image_asset_id),"%s",download->id);
    portEXIT_CRITICAL(&s_lock);
    download->requested_us=esp_timer_get_time();
    return send_text(message);
}
static uint8_t *image_decode(const image_download_t *download, const char **failure_code) {
    *failure_code="image_digest_mismatch";
    uint8_t expected[32], actual[32];
    const mbedtls_md_info_t *md=mbedtls_md_info_from_type(MBEDTLS_MD_SHA256);
    if (!decode_key(download->digest,expected) || !md ||
        mbedtls_md(md,download->jpeg,download->total,actual)!=0 || memcmp(expected,actual,sizeof(actual))) return NULL;
    *failure_code="image_decode_failed";
    jpeg_dec_config_t config=DEFAULT_JPEG_DEC_CONFIG();
    config.output_type=JPEG_PIXEL_FORMAT_RGB565_LE;
    jpeg_dec_handle_t decoder=NULL;
    if (jpeg_dec_open(&config,&decoder)!=JPEG_ERR_OK) return NULL;
    jpeg_dec_io_t io={.inbuf=download->jpeg,.inbuf_len=(int)download->total};
    jpeg_dec_header_info_t info={0};
    uint8_t *pixels=NULL;
    if (jpeg_dec_parse_header(decoder,&io,&info)==JPEG_ERR_OK &&
        info.width==download->width && info.height==download->height) {
        size_t bytes=(size_t)info.width*info.height*2;
        pixels=heap_caps_aligned_alloc(16,bytes,MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
        if (pixels) {
            io.outbuf=pixels;
            if (jpeg_dec_process(decoder,&io)!=JPEG_ERR_OK) {free(pixels);pixels=NULL;}
        } else *failure_code="image_no_memory";
    }
    jpeg_dec_close(decoder);
    return pixels;
}
static const char *image_failure_notice(const char *code) {
    if (!strcmp(code,"asset_expired")) return "图片已过期，请重新获取";
    if (!strcmp(code,"image_no_memory")) return "图片内存不足，文字仍可阅读";
    if (!strcmp(code,"image_digest_mismatch") || !strcmp(code,"image_chunk_invalid") ||
        !strcmp(code,"image_descriptor_invalid")) return "图片校验失败，文字仍可阅读";
    if (!strcmp(code,"image_decode_failed")) return "图片无法解码，文字仍可阅读";
    if (!strcmp(code,"display_unavailable")) return "图片暂时无法显示，文字仍可阅读";
    return "图片下载失败，文字仍可阅读";
}
static void image_failure_poll(image_download_t *download) {
    image_failure_t failure;
    portENTER_CRITICAL(&s_lock);
    failure=s_image_failure;
    bool fresh=s.authenticated && s.result_valid && same(failure.token,s.result_token);
    portEXIT_CRITICAL(&s_lock);
    if (!failure.pending) return;
    if (fresh) {
        if (download->jpeg && same(download->token,failure.token)) image_discard(download);
        if (!failure.local_posted) {
            bot_command_t notice={.kind=BOT_COMMAND_IMAGE_FAILED,.token=failure.token,
                .received_ms=(uint32_t)(esp_timer_get_time()/1000),.ttl_ms=2000};
            snprintf(notice.data.caption,sizeof(notice.data.caption),"%s",image_failure_notice(failure.code));
            /* Unlike a lost conversation command, an image notice must not
             * close the session or erase the answer when this queue is full. */
            if (bot_bus_submit_command(s_bus,&notice,0)!=ESP_OK) return;
            portENTER_CRITICAL(&s_lock);
            if (s_image_failure.generation==failure.generation) s_image_failure.local_posted=true;
            portEXIT_CRITICAL(&s_lock);
        }
        portENTER_CRITICAL(&s_lock);uint32_t seq=s.presentation_valid?s.segment_generation:0;portEXIT_CRITICAL(&s_lock);
        if (!brain_secure_display_report_sequence(failure.token,seq,"image",failure.asset_id,false,failure.code)) return;
    }
    portENTER_CRITICAL(&s_lock);
    if (s_image_failure.generation==failure.generation) s_image_failure.pending=false;
    portEXIT_CRITICAL(&s_lock);
}
static void image_download_failed(image_download_t *download, const char *code) {
    brain_secure_image_failed(download->token,download->id,code);
    image_discard(download);
}
static void image_poll(image_download_t *download) {
    portENTER_CRITICAL(&s_lock);uint32_t current_seq=s.presentation_valid?s.segment_generation:0;portEXIT_CRITICAL(&s_lock);
    if (download->jpeg && (!image_current(download->token) || download->presentation_sequence!=current_seq)) image_discard(download);
    image_failure_poll(download);
    image_message_t *message=NULL;
    if (s_image_messages && xQueueReceive(s_image_messages,&message,0)==pdTRUE) {
        /* Read authority after dequeue: the WSS callback may just have moved
         * COMMITTED to WAIT_NEXT and enqueued this result in the same interval. */
        portENTER_CRITICAL(&s_lock);
        bool current_sequence=message->presentation_sequence == (s.presentation_valid?s.segment_generation:0);
        portEXIT_CRITICAL(&s_lock);
        bool fresh=current_sequence && image_current(message->token) && !image_failed_for(message->token);
        if (fresh && message->kind==IMAGE_DESCRIPTOR) {
            image_discard(download);
            download->jpeg=heap_caps_malloc(message->total,MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
            if (download->jpeg) {
                download->token=message->token;download->total=message->total;
                download->presentation_sequence=message->presentation_sequence;
                download->width=message->width;download->height=message->height;
                snprintf(download->id,sizeof(download->id),"%s",message->asset_id);
                snprintf(download->digest,sizeof(download->digest),"%s",message->digest);
                if (!image_request(download)) image_download_failed(download,"image_request_failed");
            } else brain_secure_image_failed(message->token,message->asset_id,"image_no_memory");
        } else if (fresh && download->jpeg && message->kind==IMAGE_CHUNK &&
                   same(download->token,message->token)) {
            size_t decoded=0;
            bool valid=!strcmp(download->id,message->asset_id) && message->offset==download->offset &&
                message->bytes<=download->total-download->offset &&
                mbedtls_base64_decode(download->jpeg+download->offset,download->total-download->offset,&decoded,
                (const unsigned char *)message->base64,strlen(message->base64))==0 && decoded==message->bytes;
            if (valid) download->offset+=(uint32_t)decoded;
            if (!valid || message->eof!=(download->offset==download->total)) {
                image_download_failed(download,"image_chunk_invalid");
            } else if (message->eof) {
                bot_display_image_t result={.token=download->token,.width=download->width,.height=download->height,
                    .presentation_sequence=download->presentation_sequence};
                snprintf(result.asset_id,sizeof(result.asset_id),"%s",download->id);
                const char *failure_code;
                result.pixels=image_decode(download,&failure_code);
                if (!result.pixels) brain_secure_image_failed(download->token,download->id,failure_code);
                else if (!image_current(result.token)) free(result.pixels);
                else if (!s_image_results || xQueueSend(s_image_results,&result,0)!=pdTRUE) {
                    free(result.pixels);
                    brain_secure_image_failed(download->token,download->id,"image_queue_full");
                }
                image_discard(download);
            } else {
                download->retries=0;
                if (!image_request(download)) image_download_failed(download,"image_request_failed");
            }
        }
        free(message);
    }
    if (download->jpeg && esp_timer_get_time()-download->requested_us>3000000) {
        if (download->retries++==0) {
            if (!image_request(download)) image_download_failed(download,"image_request_failed");
        } else image_download_failed(download,"image_timeout");
    }
    image_failure_poll(download);
    display_report_t report;
    if (s_display_reports && xQueuePeek(s_display_reports,&report,0)==pdTRUE) {
        portENTER_CRITICAL(&s_lock);
        bool fresh=s.authenticated && s.result_valid && same(report.token,s.result_token) &&
            report.presentation_sequence == (s.presentation_valid?s.segment_generation:0);
        portEXIT_CRITICAL(&s_lock);
        if (!fresh || (report.rendered && !strcmp(report.part,"image") && image_failed_for(report.token))) {
            (void)xQueueReceive(s_display_reports,&report,0);
            return;
        }
        char id[24],runtime[180],out[512];
        request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),report.token);
        int n=snprintf(out,sizeof(out),"{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"display.report\",\"payload\":{\"runtime\":%s,\"part\":\"%s\",\"asset_id\":\"%s\",\"status\":\"%s\",\"code\":\"%s\"}}",
            id,runtime,report.part,report.asset_id,report.rendered?"rendered":"unavailable",report.code);
        /* Retain a failed send in the fixed queue while its authority is valid. */
        if (n>0 && n<sizeof(out) && send_text(out)) {
            (void)xQueueReceive(s_display_reports,&report,0);
            portENTER_CRITICAL(&s_lock);
            if (s.presentation_valid && s.segment_active && same(report.token,s.remote) &&
                report.presentation_sequence==s.segment_generation) {
                if (!strcmp(report.part,"text")) {
                    s.segment_text_done=true;
                    if (report.rendered && !s.segment_asset_id[0] && !s.segment_rendered_us)
                        s.segment_rendered_us=esp_timer_get_time();
                }
                if (!strcmp(report.part,"image") && !strcmp(report.asset_id,s.segment_asset_id)) {
                    s.segment_image_done=true;
                    if (report.rendered && !s.segment_rendered_us) s.segment_rendered_us=esp_timer_get_time();
                }
                if (!report.rendered) {
                    s.segment_failed=true;
                    snprintf(s.segment_code,sizeof(s.segment_code),"%s",report.code);
                }
            }
            portEXIT_CRITICAL(&s_lock);
        }
    }
}
static void presentation_poll(void) {
    secure_state_t state;
    portENTER_CRITICAL(&s_lock);state=s;portEXIT_CRITICAL(&s_lock);
    if (!state.presentation_valid || !state.segment_active || !state.authenticated ||
        !state.active || !same(state.remote,state.desired)) return;
    const int64_t now=esp_timer_get_time();
    const bool awaiting_display=!state.segment_text_done || !state.segment_image_done;
    int64_t limit=(state.segment_audio_done||awaiting_display)?30000000:
        (int64_t)state.playback_expected*1000000/32000+10000000;
    if (limit>120000000) limit=120000000;
    /* A short narration's playback budget starts after its actual illustration
     * flush. JPEG preparation has its own bounded 30-second ready window. */
    int64_t media_since=state.segment_rendered_us?state.segment_rendered_us:state.segment_started_us;
    const bool expired=now-(awaiting_display?state.segment_started_us:media_since)>limit || now-state.presentation_started_us>120000000 ||
        (state.segment_video && now-state.video_started_us>15000000);
    if (expired && (!state.segment_failed || !state.segment_text_done || !state.segment_image_done ||
                    !state.segment_audio_done || state.playback_id)) {
        portENTER_CRITICAL(&s_lock);
        if (s.segment_active && s.segment_generation==state.segment_generation) {
            if (!s.segment_failed) snprintf(s.segment_code,sizeof(s.segment_code),"segment_timeout");
            s.segment_failed=true;
            /* No fake rendered receipt. A failed terminal is distinguishable. */
            s.segment_text_done=s.segment_image_done=true;
            if (!s.playback_id) {s.segment_audio_done=true;s.phase=COMMITTED;}
        }
        portEXIT_CRITICAL(&s_lock);
        if (state.playback_id) bot_audio_playback_cancel(s_audio,state.playback_id);
        return;
    }
    if (!state.segment_text_done || !state.segment_image_done || !state.segment_audio_done || state.playback_id) return;
    int64_t visible_since=state.segment_rendered_us?state.segment_rendered_us:state.segment_started_us;
    if (!state.segment_failed && now-visible_since<(int64_t)state.segment_hold_ms*1000) return;
    char id[24],runtime[180],message[900];
    request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),state.remote);
    int n=snprintf(message,sizeof(message),
        "{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"presentation.segment.report\",\"payload\":{\"runtime\":%s,\"presentation_id\":\"%s\",\"output_lease_id\":\"%s\",\"segment_id\":\"%s\",\"seq\":%lu,\"presentation_index\":%lu,\"status\":\"%s\",\"code\":\"%s\"}}",
        id,runtime,state.presentation_id,state.output_lease_id,state.segment_id,
        (unsigned long)state.presentation_sequence,(unsigned long)state.presentation_index,state.segment_failed?"failed":"completed",state.segment_code);
    if (n<0 || n>=sizeof(message)) {fail_operation(state.remote,"Presentation receipt overflow");return;}
    /* Retire before send: a fast next segment can arrive on the WS task as soon
     * as the gateway receives this receipt. Never mutate that new segment. */
    portENTER_CRITICAL(&s_lock);
    bool current=s.segment_active && s.segment_generation==state.segment_generation &&
        s.active && same(s.remote,state.remote) && same(s.desired,state.remote);
    if (current) {s.segment_active=false;s.phase=(state.segment_final||state.segment_failed)?WAIT_NEXT:COMMITTED;}
    portEXIT_CRITICAL(&s_lock);
    if (!current) return;
    if (!send_text(message)) {
        portENTER_CRITICAL(&s_lock);
        if (s.presentation_valid && s.segment_generation==state.segment_generation && same(s.remote,state.remote)) {
            s.segment_active=true;s.phase=COMMITTED;
        }
        portEXIT_CRITICAL(&s_lock);
        return;
    }
    /* A failed terminal never opens follow-up ahead of the gateway's matching
     * cancellation. Retire locally too if that control packet is delayed. */
    if (state.segment_failed) fail_operation(state.remote,"Presentation segment failed");
    else if (state.segment_final) command(BOT_COMMAND_PRESENTATION_DONE,state.remote,NULL);
}

static bool send_packet(const pcm_frame_t *packet,uint32_t stream,uint32_t sequence) {
    unsigned char wire[4+1+20+PCM_SAMPLES*2+PLAIN_MAC_SIZE]={0};
    unsigned char *body=wire+4,*bytes=body+1;body[0]='I';memcpy(bytes,"XDAI",4);
    write32(bytes+4,stream); write32(bytes+8,sequence);
    for (unsigned i=0;i<8;i++) bytes[12+i]=(unsigned char)(packet->timestamp_ms>>(8*i));
    for (size_t i=0;i<packet->count;i++) {
        uint16_t sample=(uint16_t)packet->pcm[i]; bytes[20+2*i]=(unsigned char)sample; bytes[21+2*i]=(unsigned char)(sample>>8);
    }
    size_t content=1+20+packet->count*2, length=content+PLAIN_MAC_SIZE;
    wire[0]=(uint8_t)(length>>24);wire[1]=(uint8_t)(length>>16);wire[2]=(uint8_t)(length>>8);wire[3]=(uint8_t)length;
    bool authenticated=plain_hmac(body,content,body+content);
    if (!authenticated) ESP_LOGW(TAG,"Plain audio frame authentication failed");
    bool ok=authenticated && plain_send_all(wire,4+length,packet->token);
    memset(wire,0,sizeof(wire));return ok;
}
static void secure_task(void *unused) {
    (void)unused;
    bool heard=false, ending=false, diagnostic_paused=false;
    int64_t ending_us=0, started_us=0, diagnostic_started_us=0;
    image_download_t download={0};
    for (;;) {
        if (!playback_event_poll()) {vTaskDelay(pdMS_TO_TICKS(20));continue;}
        image_poll(&download);
        presentation_poll();
        secure_state_t state;
        portENTER_CRITICAL(&s_lock); state=s; portEXIT_CRITICAL(&s_lock);
        if (atomic_exchange(&s_playback_event_overflow,false)) {
            fail_session("Hardware playback event queue overflow");
            /* The missing terminal cannot keep an otherwise revoked network
             * lease forever. fail_session already cancelled its exact ID. */
            portENTER_CRITICAL(&s_lock);s.playback_id=0;portEXIT_CRITICAL(&s_lock);
            continue;
        }
        if (state.playback_id && (!state.authenticated || !state.active || !same(state.desired,state.playback_token))) {
            if (!state.playback_cancel_requested) {
                portENTER_CRITICAL(&s_lock);
                if (s.playback_id==state.playback_id) s.playback_cancel_requested=true;
                portEXIT_CRITICAL(&s_lock);
                bot_audio_playback_cancel(s_audio,state.playback_id);
            }
            /* The terminal hardware report must precede the next audio.begin.
             * Root may already have reserved a newer token; never relabel it. */
            if (state.authenticated) {vTaskDelay(pdMS_TO_TICKS(10));continue;}
        }
        if (state.authenticated && !state.active && diagnostic_paused) {
            if (bot_lan_resume_auto()==ESP_OK) diagnostic_paused=false;
        }
        if (!state.authenticated) { plain_close();flush_pcm(); heard=false; ending=false; vTaskDelay(pdMS_TO_TICKS(50)); continue; }
        if (atomic_exchange(&s_overflow,false)) {
            portENTER_CRITICAL(&s_lock);bot_session_token_t token=s_overflow_token;portEXIT_CRITICAL(&s_lock);
            flush_pcm();fail_operation(token,"Secure audio queue overflow");continue;
        }
        if (!state.active) {
            if (state.phase==CAPTURING) capture_event(state.remote,false);
            plain_close();
            if (state.phase!=READY && state.phase!=WELCOME) {
                flush_pcm();
                if (!simple_request("session.close")) {vTaskDelay(pdMS_TO_TICKS(30));continue;}
                portENTER_CRITICAL(&s_lock); if (s.authenticated) {s.phase=READY;s.remote=(bot_session_token_t){0};s.audio_allowed=false;} portEXIT_CRITICAL(&s_lock);
            }
            heard=false;ending=false;vTaskDelay(pdMS_TO_TICKS(30));continue;
        }
        if (state.phase==READY) {
            if (!state.desired.session_id || state.activation==BOT_ACTIVATION_NONE) {vTaskDelay(pdMS_TO_TICKS(30));continue;}
            if (!state.silent_supported) {
                finish_with_message(state.desired,"请更新 Mac 小呆服务",false);
                continue;
            }
            /* The optional diagnostic WebSocket consumes internal RAM needed
             * by real-time Wi-Fi media. Pause it for the explicit interaction
             * and let its worker reconnect after the session finishes. */
            if (!diagnostic_paused && bot_lan_state()!=BOT_LAN_STOPPED) {
                if (bot_lan_disconnect()!=ESP_OK) {fail_operation(state.desired,"Could not pause diagnostic link for audio");continue;}
                diagnostic_paused=true;diagnostic_started_us=esp_timer_get_time();
            }
            if (diagnostic_paused && bot_lan_state()!=BOT_LAN_DISCONNECTED &&
                bot_lan_state()!=BOT_LAN_STOPPED) {
                if (esp_timer_get_time()-diagnostic_started_us>5000000) fail_operation(state.desired,"Diagnostic link did not pause in time");
                else vTaskDelay(pdMS_TO_TICKS(30));
                continue;
            }
            char id[24], runtime[180], msg[1100]; request_id(id,sizeof(id)); runtime_json(runtime,sizeof(runtime),state.desired);
            const char *activation=state.activation==BOT_ACTIVATION_WAKE_WORD?"wake_word":"touch";
            bool spoken=state.spoken_answers && state.audio_supported;
            snprintf(msg,sizeof(msg),"{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"session.open\",\"payload\":{\"activation\":\"%s\",\"keyword\":\"小呆小呆\",\"runtime\":%s,\"output\":{\"version\":1,\"audio\":%s,\"cards\":[\"text\",\"image\"],\"image\":{\"mime\":\"image/jpeg\",\"max_width\":480,\"max_height\":272,\"max_bytes\":65536},\"presentation\":{\"version\":1,\"max_segments\":64,\"max_duration_ms\":120000,\"frame_ack\":true},\"video\":{\"mime\":\"video/x-motion-jpeg\",\"transport\":\"jpeg_frames\",\"max_width\":480,\"max_height\":272,\"max_frame_bytes\":65536,\"max_fps\":5,\"max_duration_ms\":15000}}}}",id,activation,runtime,spoken?"true":"false");
            portENTER_CRITICAL(&s_lock); s.remote=state.desired;s.session_spoken=spoken;s.cancel_before_next=false;snprintf(s.open_id,sizeof(s.open_id),"%s",id);s.phase=OPENING;portEXIT_CRITICAL(&s_lock);
            if (state.spoken_answers && !state.audio_supported) ESP_LOGW(TAG,"Mac lacks audio.reply.v1; continuing with text/image replies");
            if (!send_text(msg)) fail_operation(state.desired,"Could not open secure session");
        } else if (state.remote.session_id!=state.desired.session_id) {
            capture_event(state.remote,false);
            plain_close();flush_pcm();
            if (!simple_request("session.close")) {vTaskDelay(pdMS_TO_TICKS(30));continue;}
            portENTER_CRITICAL(&s_lock);s.phase=READY;s.audio_allowed=false;s.remote=(bot_session_token_t){0};portEXIT_CRITICAL(&s_lock);
            heard=false;ending=false;
        } else if (!same(state.remote,state.desired)) {
            /* Runtime invalidates the old turn before publishing the new one. */
            capture_event(state.remote,false);
            plain_close();flush_pcm();
            if (!(state.phase==WAIT_NEXT && state.followup_armed && !state.cancel_before_next)) {
                if (!simple_request("assistant.cancel")) {vTaskDelay(pdMS_TO_TICKS(30));continue;}
            }
            portENTER_CRITICAL(&s_lock);s.remote=state.desired;s.audio_allowed=false;s.cancel_before_next=false;s.phase=OPEN;portEXIT_CRITICAL(&s_lock);
            heard=false;ending=false;
        } else if (state.phase==OPEN) {
            char id[24], runtime[180], msg[500]; request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),state.desired);
            uint32_t stream=esp_random();if (!stream) stream=1;
            snprintf(msg,sizeof(msg),"{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"audio.begin\",\"payload\":{\"stream_id\":%lu,\"format\":\"pcm_s16le\",\"sample_rate\":16000,\"channels\":1,\"transport\":\"plain_tcp_v1\",\"runtime\":%s}}",id,(unsigned long)stream,runtime);
            portENTER_CRITICAL(&s_lock);s.stream=stream;s.sequence=0;snprintf(s.begin_id,sizeof(s.begin_id),"%s",id);s.phase=BEGINNING;portEXIT_CRITICAL(&s_lock);
            if (!send_text(msg)) fail_operation(state.remote,"Could not start secure audio");
            heard=false;ending=false;started_us=0;
        } else if (state.phase==PLAIN_CONNECTING) {
            if (!plain_connect(&state)) fail_operation(state.remote,"Plain audio connection failed");
            else {
                portENTER_CRITICAL(&s_lock);
                bool ready=s.phase==PLAIN_CONNECTING && s.authenticated && s.active &&
                    same(s.desired,state.remote) && same(s.remote,state.remote);
                if (ready) {
                    memset(s.plain_key,0,sizeof(s.plain_key));
                    s.phase=CAPTURING;s.audio_allowed=true;
                }
                portEXIT_CRITICAL(&s_lock);
                if (ready) {
                    ESP_LOGI(TAG,"Plain LAN audio ready; runtime retains PCM permission");
                    capture_event(state.remote,true);
                } else plain_close();
                started_us=0;
            }
        } else if (state.phase==CAPTURING) {
            if (state.utterance) {heard=true;ending=false;if (!started_us) started_us=esp_timer_get_time();}
            else if (heard && !ending) {ending=true;ending_us=esp_timer_get_time();}
            pcm_frame_t packet;
            if (xQueueReceive(s_pcm,&packet,pdMS_TO_TICKS(20))==pdTRUE) {
                portENTER_CRITICAL(&s_lock); bool allowed=s.authenticated&&s.active&&s.audio_allowed&&s.pcm_permitted&&s.phase==CAPTURING&&same(packet.token,s.desired)&&same(packet.token,s.remote);uint32_t seq=s.sequence,stream=s.stream;portEXIT_CRITICAL(&s_lock);
                if (allowed && !send_packet(&packet,stream,seq)) fail_operation(packet.token,"Secure audio send failed");
                else if (allowed) {
                    if (!started_us) started_us=esp_timer_get_time();
                    heard=true;
                    portENTER_CRITICAL(&s_lock);if (s.phase==CAPTURING && same(packet.token,s.remote)) s.sequence++;portEXIT_CRITICAL(&s_lock);
                }
                memset(&packet,0,sizeof(packet));
            }
            if (started_us && esp_timer_get_time()-started_us>29000000) { fail_operation(state.remote,"Speech capture timed out"); }
            else if (ending && esp_timer_get_time()-ending_us>200000 && uxQueueMessagesWaiting(s_pcm)==0) {
                char id[24],runtime[180],msg[400];request_id(id,sizeof(id));runtime_json(runtime,sizeof(runtime),state.remote);
                uint32_t frames;
                portENTER_CRITICAL(&s_lock);frames=s.sequence;portEXIT_CRITICAL(&s_lock);
                snprintf(msg,sizeof(msg),"{\"v\":3,\"type\":\"request\",\"id\":\"%s\",\"method\":\"audio.commit\",\"payload\":{\"stream_id\":%lu,\"frames\":%lu,\"runtime\":%s}}",id,(unsigned long)state.stream,(unsigned long)frames,runtime);
                portENTER_CRITICAL(&s_lock);
                bool current=s.authenticated && s.active && s.phase==CAPTURING &&
                    same(s.desired,state.remote) && same(s.remote,state.remote);
                if (current) {
                    s.audio_allowed=false;s.progress_seq=0;s.commit_started_us=esp_timer_get_time();
                    snprintf(s.commit_id,sizeof(s.commit_id),"%s",id);s.phase=COMMITTED;
                }
                portEXIT_CRITICAL(&s_lock);
                if (!current) continue;
                const bot_event_t submitted = {.kind=BOT_EVENT_TURN_SUBMITTED,
                    .source=BOT_SOURCE_LINK,.observed_ms=(uint32_t)(esp_timer_get_time()/1000),
                    .token=state.remote};
                if (bot_bus_post(s_bus,&submitted,0)!=ESP_OK) {
                    fail_operation(state.remote,"Could not seal microphone capture");
                    continue;
                }
                capture_event(state.remote,false);
                if (!send_text(msg)) fail_operation(state.remote,"Could not submit secure audio");
                else ESP_LOGI(TAG,"Plain LAN audio submitted (%lu frames)",(unsigned long)frames);
                heard=false;ending=false;
            }
        } else if (state.phase==COMMITTED && esp_timer_get_time()-state.commit_started_us>=180000000) {
            bool cancelled=simple_request("assistant.cancel");
            portENTER_CRITICAL(&s_lock);s.cancel_before_next=!cancelled;portEXIT_CRITICAL(&s_lock);
            finish_with_message(state.remote,"处理超时，请再试一次",true);
        } else if (state.phase==PLAYBACK_PENDING) {
            playback_prepare(&state);
            vTaskDelay(pdMS_TO_TICKS(10));
        } else if (state.phase==PLAYING) {
            if (!plain_receive()) fail_operation(state.remote,"Plain audio response failed");
            vTaskDelay(pdMS_TO_TICKS(10));
        } else if (state.phase==DRAINING) {
            if (!state.presentation_valid) plain_close();
            if (esp_timer_get_time()-state.playback_started_us>
                (int64_t)state.playback_expected*1000000/32000+10000000)
                fail_operation(state.remote,"Hardware playback completion timed out");
            vTaskDelay(pdMS_TO_TICKS(10));
        } else if (state.phase==WAIT_NEXT) {
            plain_close();vTaskDelay(pdMS_TO_TICKS(20));
        } else vTaskDelay(pdMS_TO_TICKS(20));
    }
}

esp_err_t brain_secure_start(bot_bus_t *bus,bot_audio_t *audio) {
    if (!bus || !audio || s_bus) return ESP_ERR_INVALID_ARG;
    s_bus=bus;s_audio=audio;
    nvs_handle_t nvs;esp_err_t err=nvs_open("espbot_secure",NVS_READONLY,&nvs);
    if (err==ESP_ERR_NVS_NOT_FOUND) {ESP_LOGI(TAG,"Secure gateway has not been provisioned");return ESP_OK;}
    if (err!=ESP_OK) return err;
    size_t size=sizeof(s_provision);err=nvs_get_blob(nvs,"server",&s_provision,&size);nvs_close(nvs);
    if (err!=ESP_OK || size!=sizeof(s_provision) || !valid_provision(&s_provision)) {memset(&s_provision,0,sizeof(s_provision));return ESP_ERR_INVALID_STATE;}
    if (!load_tls_identity()) return ESP_ERR_INVALID_STATE;
    char headers[180];snprintf(headers,sizeof(headers),"Authorization: Bearer %s\r\nX-Espbot-Device: %s\r\n",s_provision.token,CONFIG_ESPBOT_DEVICE_ID);
    esp_websocket_client_config_t config={.uri=s_provision.url,.cert_pem=s_provision.cert,.cert_common_name=s_tls_name,.headers=headers,
        .buffer_size=2048,.task_stack=8192,.reconnect_timeout_ms=5000,.network_timeout_ms=10000,
        .enable_close_reconnect=true};
    s_rx=heap_caps_malloc(RX_SIZE+1,MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
    s_pcm_storage=heap_caps_malloc(PCM_QUEUE_FRAMES*sizeof(pcm_frame_t),MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
    s_plain_rx=heap_caps_malloc(PLAIN_RX_SIZE,MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT);
    if (!s_rx || !s_pcm_storage || !s_plain_rx) {heap_caps_free(s_rx);heap_caps_free(s_pcm_storage);heap_caps_free(s_plain_rx);s_rx=NULL;s_pcm_storage=NULL;s_plain_rx=NULL;memset(headers,0,sizeof(headers));return ESP_ERR_NO_MEM;}
    s_ws=esp_websocket_client_init(&config);memset(headers,0,sizeof(headers));
    if (!s_ws) {heap_caps_free(s_rx);heap_caps_free(s_pcm_storage);heap_caps_free(s_plain_rx);s_rx=NULL;s_pcm_storage=NULL;s_plain_rx=NULL;return ESP_ERR_NO_MEM;}
    s_pcm=xQueueCreateStatic(PCM_QUEUE_FRAMES,sizeof(pcm_frame_t),s_pcm_storage,&s_pcm_control);
    if (!s_pcm) {esp_websocket_client_destroy(s_ws);s_ws=NULL;heap_caps_free(s_rx);heap_caps_free(s_pcm_storage);heap_caps_free(s_plain_rx);s_rx=NULL;s_pcm_storage=NULL;s_plain_rx=NULL;return ESP_ERR_NO_MEM;}
    s_image_messages=xQueueCreate(2,sizeof(image_message_t *));
    s_image_results=xQueueCreate(1,sizeof(bot_display_image_t));
    s_display_reports=xQueueCreate(4,sizeof(display_report_t));
    s_playback_events=xQueueCreate(8,sizeof(playback_event_t));
    if (!s_image_messages || !s_image_results || !s_display_reports || !s_playback_events) {
        if (s_image_messages) vQueueDelete(s_image_messages);
        if (s_image_results) vQueueDelete(s_image_results);
        if (s_display_reports) vQueueDelete(s_display_reports);
        if (s_playback_events) vQueueDelete(s_playback_events);
        s_image_messages=NULL;s_image_results=NULL;s_display_reports=NULL;
        s_playback_events=NULL;
        vQueueDelete(s_pcm);s_pcm=NULL;esp_websocket_client_destroy(s_ws);s_ws=NULL;
        heap_caps_free(s_rx);heap_caps_free(s_pcm_storage);heap_caps_free(s_plain_rx);
        s_rx=NULL;s_pcm_storage=NULL;s_plain_rx=NULL;return ESP_ERR_NO_MEM;
    }
    err=esp_websocket_register_events(s_ws,WEBSOCKET_EVENT_ANY,ws_event,NULL);
    atomic_store(&s_discovery_enabled,true);
    if (err==ESP_OK) err=esp_websocket_client_start(s_ws);
    if (err==ESP_OK && xTaskCreateWithCaps(secure_task,"secure_media",12288,NULL,5,NULL,MALLOC_CAP_SPIRAM|MALLOC_CAP_8BIT)!=pdPASS) err=ESP_ERR_NO_MEM;
    if (err==ESP_OK) ESP_LOGI(TAG,"Secure client tasks started; internal_free=%u largest=%u",
        (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL|MALLOC_CAP_8BIT),
        (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL|MALLOC_CAP_8BIT));
    if (err!=ESP_OK) {
        atomic_store(&s_discovery_enabled,false);
        esp_websocket_client_stop(s_ws);esp_websocket_client_destroy(s_ws);s_ws=NULL;
        vQueueDelete(s_pcm);s_pcm=NULL;
        vQueueDelete(s_image_messages);vQueueDelete(s_image_results);vQueueDelete(s_display_reports);vQueueDelete(s_playback_events);
        s_image_messages=NULL;s_image_results=NULL;s_display_reports=NULL;
        s_playback_events=NULL;
        heap_caps_free(s_rx);heap_caps_free(s_pcm_storage);heap_caps_free(s_plain_rx);s_rx=NULL;s_pcm_storage=NULL;s_plain_rx=NULL;
    }
    return err;
}
