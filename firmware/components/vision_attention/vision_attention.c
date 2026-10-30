#include "vision_attention.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    bool active,confirmed,visible,entering,exiting,has_positive,ambiguous;
    uint8_t hits;
    uint32_t id,first_ms,last_ms,enter_ms,exit_ms,last_positive_ms,look_start_ms;
    float vx,vy; /* center velocity, normalized image units per ms */
    float eye_vx,eye_vy; /* human eye-midpoint velocity, normalized / ms */
    vatt_state_t status;
    vatt_gaze_t evidence;
    uint32_t looks;
    uint64_t completed_ms;
    vatt_observation_t obs;
} slot_t;

struct vatt_tracker {
    vatt_config_t cfg;
    slot_t slots[VATT_MAX_FACES];
    uint32_t next_id,last_frame_ms,last_face_ms,primary_id;
    bool has_frame,has_seen_face,camera_ok;
};

static float minf(float a,float b) {return a < b ? a : b;}
static float maxf(float a,float b) {return a > b ? a : b;}
static float clampf(float v,float a,float b) {return minf(b,maxf(a,v));}
static uint32_t elapsed(uint32_t now,uint32_t start) {return now-start;}
static vatt_point_t center(vatt_box_t b) {
    return (vatt_point_t){(b.left+b.right)*.5f,(b.top+b.bottom)*.5f};
}
static float width(vatt_box_t b) {return b.right-b.left;}
static float height(vatt_box_t b) {return b.bottom-b.top;}
static bool valid_obs(const vatt_observation_t *o,const vatt_config_t *c) {
    if (!o || !isfinite(o->face_confidence) || o->face_confidence < c->min_face_confidence ||
        o->face_confidence > 1 || !isfinite(o->bbox.left) || !isfinite(o->bbox.top) ||
        !isfinite(o->bbox.right) || !isfinite(o->bbox.bottom) ||
        o->bbox.left < 0 || o->bbox.top < 0 || o->bbox.right > 1 ||
        o->bbox.bottom > 1 || width(o->bbox) <= 0 || height(o->bbox) <= 0)
        return false;
    if (!isfinite(o->left_eye.x) || !isfinite(o->left_eye.y) ||
        !isfinite(o->right_eye.x) || !isfinite(o->right_eye.y))
        return false;
    if (o->left_eye.x < 0 || o->left_eye.x > 1 || o->left_eye.y < 0 ||
        o->left_eye.y > 1 || o->right_eye.x < 0 || o->right_eye.x > 1 ||
        o->right_eye.y < 0 || o->right_eye.y > 1) return false;
    if ((o->left_pupil_valid &&
         (!isfinite(o->left_pupil.x) || !isfinite(o->left_pupil.y) ||
          o->left_pupil.x < 0 || o->left_pupil.x > 1 ||
          o->left_pupil.y < 0 || o->left_pupil.y > 1)) ||
        (o->right_pupil_valid &&
         (!isfinite(o->right_pupil.x) || !isfinite(o->right_pupil.y) ||
          o->right_pupil.x < 0 || o->right_pupil.x > 1 ||
          o->right_pupil.y < 0 || o->right_pupil.y > 1)))
        return false;
    if ((o->head_pose_valid &&
         (!isfinite(o->head_yaw_deg) || !isfinite(o->head_pitch_deg))) ||
        (o->gaze_direction_valid &&
         (!isfinite(o->gaze_yaw_deg) || !isfinite(o->gaze_pitch_deg))))
        return false;
    if (o->eye_contact_valid &&
        (!isfinite(o->eye_contact_score) || o->eye_contact_score < 0 ||
         o->eye_contact_score > 1 || !isfinite(o->gaze_quality) ||
         o->gaze_quality < 0 || o->gaze_quality > 1))
        return false;
    return true;
}
static void event(vatt_snapshot_t *out,vatt_event_kind_t kind,
                  uint32_t id,uint32_t ms,uint32_t duration) {
    if (out->event_count >= VATT_MAX_EVENTS) return;
    out->events[out->event_count++] = (vatt_event_t){kind,id,ms,duration};
}
vatt_config_t vatt_default_config(void) {
    return (vatt_config_t){350,450,250,550,800,.72f,.35f,.45f,.45f};
}
vatt_tracker_t *vatt_tracker_create(const vatt_config_t *cfg) {
    vatt_config_t c=cfg ? *cfg : vatt_default_config();
    if (!c.look_enter_ms || !c.look_exit_ms || !c.unknown_grace_ms ||
        !c.no_face_ms || c.track_expire_ms <= c.unknown_grace_ms ||
        !isfinite(c.positive_score) || !isfinite(c.negative_score) ||
        !isfinite(c.min_gaze_quality) || !isfinite(c.min_face_confidence) ||
        c.negative_score >= c.positive_score ||
        c.negative_score < 0 || c.positive_score > 1 ||
        c.min_gaze_quality < 0 || c.min_gaze_quality > 1 ||
        c.min_face_confidence <= 0 || c.min_face_confidence > 1)
        return NULL;
    vatt_tracker_t *t=(vatt_tracker_t *)calloc(1,sizeof(*t));
    if (t) { t->cfg=c; t->next_id=1; t->camera_ok=true; }
    return t;
}
void vatt_tracker_destroy(vatt_tracker_t *t) {free(t);}

static void end_look(slot_t *s,uint32_t time,vatt_snapshot_t *out) {
    if (s->status != VATT_LOOKING_AT_BOT) return;
    uint32_t duration=elapsed(time,s->look_start_ms);
    s->completed_ms += duration;
    s->status=VATT_FACE_PRESENT;
    s->exiting=false;
    s->entering=false;
    event(out,VATT_LOOK_END,s->id,time,duration);
}
static vatt_gaze_t gaze_evidence(const vatt_observation_t *o,
                                const vatt_config_t *cfg,bool ambiguous) {
    if (ambiguous || !o->eye_contact_valid || o->gaze_quality < cfg->min_gaze_quality)
        return VATT_GAZE_UNKNOWN;
    if (o->eye_contact_score >= cfg->positive_score) return VATT_GAZE_AT_BOT;
    if (o->eye_contact_score <= cfg->negative_score) return VATT_GAZE_AWAY;
    return VATT_GAZE_UNKNOWN;
}
static void update_look(slot_t *s,uint32_t now,
                        const vatt_config_t *cfg,vatt_snapshot_t *out) {
    if (!s->confirmed) return;
    if (s->evidence == VATT_GAZE_AT_BOT) {
        s->has_positive=true;
        s->last_positive_ms=now;
        s->exiting=false;
        if (s->status != VATT_LOOKING_AT_BOT) {
            if (!s->entering) {s->entering=true;s->enter_ms=now;}
            if (elapsed(now,s->enter_ms) >= cfg->look_enter_ms) {
                s->status=VATT_LOOKING_AT_BOT;
                s->look_start_ms=s->enter_ms;
                ++s->looks;
                s->entering=false;
                event(out,VATT_LOOK_BEGIN,s->id,now,0);
            }
        }
    } else if (s->evidence == VATT_GAZE_AWAY) {
        s->entering=false;
        if (s->status == VATT_LOOKING_AT_BOT) {
            if (!s->exiting) {s->exiting=true;s->exit_ms=now;}
            if (elapsed(now,s->exit_ms) >= cfg->look_exit_ms)
                end_look(s,s->exit_ms,out); /* stop clock at first verified look-away */
        }
    } else {
        s->entering=false;
        s->exiting=false;
        if (s->status == VATT_LOOKING_AT_BOT &&
            (!s->has_positive ||
             elapsed(now,s->last_positive_ms) >= cfg->unknown_grace_ms))
            end_look(s,s->last_positive_ms+cfg->unknown_grace_ms,out);
    }
}
static float association_cost(const slot_t *s,const vatt_observation_t *o,uint32_t now) {
    vatt_point_t a=center(s->obs.bbox),b=center(o->bbox);
    uint32_t dt=elapsed(now,s->last_ms);
    float horizon=minf((float)dt,180.0f);
    float dx=b.x-(a.x+s->vx*horizon),dy=b.y-(a.y+s->vy*horizon);
    float scale=maxf(.06f,maxf(width(s->obs.bbox),width(o->bbox)));
    float norm=sqrtf(dx*dx+dy*dy)/scale;
    float width_delta=fabsf(width(o->bbox)-width(s->obs.bbox))/scale;
    if (norm>1.45f || width_delta>1.0f) return 1000.0f;
    return norm+.25f*width_delta;
}
static void smooth_observation(slot_t *s,const vatt_observation_t *o,uint32_t now) {
    vatt_point_t old=center(s->obs.bbox),fresh=center(o->bbox);
    vatt_point_t eye_old={(s->obs.left_eye.x+s->obs.right_eye.x)*.5f,
                          (s->obs.left_eye.y+s->obs.right_eye.y)*.5f};
    vatt_point_t eye_new={(o->left_eye.x+o->right_eye.x)*.5f,
                          (o->left_eye.y+o->right_eye.y)*.5f};
    uint32_t dt=elapsed(now,s->last_ms);
    if (dt && dt<1000) {
        float vx=(fresh.x-old.x)/(float)dt,vy=(fresh.y-old.y)/(float)dt;
        s->vx=clampf(.65f*s->vx+.35f*vx,-.005f,.005f);
        s->vy=clampf(.65f*s->vy+.35f*vy,-.005f,.005f);
        s->eye_vx=clampf(.6f*s->eye_vx+
            .4f*(eye_new.x-eye_old.x)/(float)dt,-.005f,.005f);
        s->eye_vy=clampf(.6f*s->eye_vy+
            .4f*(eye_new.y-eye_old.y)/(float)dt,-.005f,.005f);
    }
    vatt_observation_t v=*o;
    float alpha=dt >= 180 ? .85f : .68f;
#define BLEND(field) (alpha*(o->field)+(1.0f-alpha)*(s->obs.field))
    v.bbox.left=BLEND(bbox.left);v.bbox.top=BLEND(bbox.top);
    v.bbox.right=BLEND(bbox.right);v.bbox.bottom=BLEND(bbox.bottom);
    v.left_eye.x=BLEND(left_eye.x);v.left_eye.y=BLEND(left_eye.y);
    v.right_eye.x=BLEND(right_eye.x);v.right_eye.y=BLEND(right_eye.y);
    if (o->left_pupil_valid && s->obs.left_pupil_valid) {
        v.left_pupil.x=BLEND(left_pupil.x);v.left_pupil.y=BLEND(left_pupil.y);
    }
    if (o->right_pupil_valid && s->obs.right_pupil_valid) {
        v.right_pupil.x=BLEND(right_pupil.x);v.right_pupil.y=BLEND(right_pupil.y);
    }
#undef BLEND
    s->obs=v;
}
static void observe(slot_t *s,const vatt_observation_t *o,
                    uint32_t now,bool ambiguous,const vatt_config_t *cfg,
                    vatt_snapshot_t *out) {
    if (s->active) {
        uint32_t gap=elapsed(now,s->last_ms);
        if (gap > cfg->unknown_grace_ms) {
            /* A 400ms inference gap is not continuous gaze evidence.
               Close ongoing fixation, or restart the entering dwell. */
            if (s->status == VATT_LOOKING_AT_BOT) {
                uint32_t cut=s->has_positive ?
                    s->last_positive_ms+cfg->unknown_grace_ms : s->last_ms;
                end_look(s,cut,out);
            }
            s->entering=false;
            s->exiting=false;
        }
        smooth_observation(s,o,now);
        s->hits=(gap > 330) ? 1 : (s->hits < 255 ? s->hits+1 : 255);
    } else {
        memset(s,0,sizeof(*s));
        s->active=true;s->first_ms=now;s->status=VATT_FACE_PRESENT;
        s->obs=*o;s->hits=1;
    }
    s->visible=true;s->last_ms=now;s->ambiguous=ambiguous;
    if (!s->confirmed && s->hits>=2) {
        s->confirmed=true;
        event(out,VATT_FACE_ENTER,s->id,now,0);
    }
    s->evidence=gaze_evidence(o,cfg,ambiguous);
    update_look(s,now,cfg,out);
}
static void expire(slot_t *s,uint32_t now,const vatt_config_t *cfg,
                   vatt_snapshot_t *out) {
    if (!s->active) return;
    if (s->status==VATT_LOOKING_AT_BOT &&
        elapsed(now,s->last_ms)>=cfg->unknown_grace_ms) {
        uint32_t cut=s->has_positive ?
            s->last_positive_ms+cfg->unknown_grace_ms : s->last_ms;
        end_look(s,cut,out);
    }
    if (elapsed(now,s->last_ms)>cfg->track_expire_ms) {
        if (s->confirmed) event(out,VATT_FACE_EXIT,s->id,now,0);
        memset(s,0,sizeof(*s));
    }
}
static uint32_t primary(vatt_tracker_t *t) {
    slot_t *old=NULL,*best=NULL;
    for (int i=0;i<VATT_MAX_FACES;i++) {
        slot_t *s=&t->slots[i];
        if (!s->active||!s->confirmed||!s->visible) continue;
        if (s->id==t->primary_id) old=s;
        if (!best || (s->status==VATT_LOOKING_AT_BOT &&
                       best->status!=VATT_LOOKING_AT_BOT))
            best=s;
        else if (best->status==s->status && !old) {
            vatt_point_t a=center(s->obs.bbox),b=center(best->obs.bbox);
            float score_a=width(s->obs.bbox)*height(s->obs.bbox)-
                          .04f*(fabsf(a.x-.5f)+fabsf(a.y-.5f));
            float score_b=width(best->obs.bbox)*height(best->obs.bbox)-
                          .04f*(fabsf(b.x-.5f)+fabsf(b.y-.5f));
            if (score_a>score_b) best=s;
        }
    }
    if (old && (!best || old->status==VATT_LOOKING_AT_BOT ||
                best->status!=VATT_LOOKING_AT_BOT)) return old->id;
    return best ? best->id : 0;
}
bool vatt_tracker_update(vatt_tracker_t *t,const vatt_observation_t *input,
                         size_t count,uint32_t now,bool camera_ok,
                         vatt_snapshot_t *out) {
    if (!t || !out || (count && !input) ||
        (t->has_frame && (int32_t)(now-t->last_frame_ms)<0)) return false;
    memset(out,0,sizeof(*out));
    out->timestamp_ms=now;
    if (t->has_frame && t->camera_ok!=camera_ok)
        event(out,camera_ok?VATT_CAMERA_RECOVERED:VATT_CAMERA_LOST,0,now,0);
    t->last_frame_ms=now;t->has_frame=true;t->camera_ok=camera_ok;
    bool used_slots[VATT_MAX_FACES]={0},used_obs[VATT_MAX_FACES]={0};
    size_t n=camera_ok ? (count<VATT_MAX_FACES ? count:VATT_MAX_FACES):0;
    /* Drop invalid face records, but do not silently interpret missing
       calibrated gaze evidence as a negative eye-contact judgement. */
    for (size_t i=0;i<n;i++) if (!valid_obs(&input[i],&t->cfg)) used_obs[i]=true;
    for (int k=0;k<VATT_MAX_FACES;k++) {
        float best_cost=1000.0f;int best_s=-1,best_o=-1;
        for (int s=0;s<VATT_MAX_FACES;s++) {
            slot_t *slot=&t->slots[s];
            if (!slot->active||used_slots[s]||
                elapsed(now,slot->last_ms)>t->cfg.track_expire_ms) continue;
            for (size_t i=0;i<n;i++) {
                if (used_obs[i]) continue;
                float cost=association_cost(slot,&input[i],now);
                if (cost<best_cost) {best_cost=cost;best_s=s;best_o=(int)i;}
            }
        }
        if (best_s<0 || best_cost>=1.45f) break;
        float next_best=1000;
        for(int s=0;s<VATT_MAX_FACES;s++) {
            if(s==best_s||used_slots[s]||!t->slots[s].active||
               elapsed(now,t->slots[s].last_ms)>t->cfg.track_expire_ms) continue;
            next_best=minf(next_best,association_cost(&t->slots[s],&input[best_o],now));
        }
        bool ambiguous=(next_best-best_cost)<.13f;
        used_slots[best_s]=true;used_obs[best_o]=true;
        observe(&t->slots[best_s],&input[best_o],now,ambiguous,&t->cfg,out);
        t->last_face_ms=now;t->has_seen_face=true;
    }
    for(size_t i=0;i<n;i++) {
        if(used_obs[i]) continue;
        int free_slot=-1;
        for(int s=0;s<VATT_MAX_FACES;s++) {
            if(!used_slots[s] && (!t->slots[s].active ||
                elapsed(now,t->slots[s].last_ms)>t->cfg.track_expire_ms)) {
                free_slot=s;break;
            }
        }
        if(free_slot<0) continue;
        slot_t *slot=&t->slots[free_slot];
        if(slot->active) {
            expire(slot,now,&t->cfg,out);
            if(slot->active) continue;
        }
        observe(slot,&input[i],now,false,&t->cfg,out);
        slot->id=t->next_id++;
        if(!t->next_id) t->next_id=1;
        used_slots[free_slot]=true;
        t->last_face_ms=now;t->has_seen_face=true;
    }
    for (int s=0;s<VATT_MAX_FACES;s++) {
        if (!used_slots[s]) t->slots[s].visible=false;
        expire(&t->slots[s],now,&t->cfg,out);
    }
    if(camera_ok) {
        for (int s=0;s<VATT_MAX_FACES;s++) {
            slot_t *slot=&t->slots[s];
            if(!slot->active||!slot->confirmed||!slot->visible) continue;
            vatt_face_t *face=&out->faces[out->face_count++];
            face->track_id=slot->id;face->visible=slot->visible;
            face->association_ambiguous=slot->ambiguous;
            face->first_seen_ms=slot->first_ms;face->last_seen_ms=slot->last_ms;
            face->stale_ms=elapsed(now,slot->last_ms);
            face->eye_midpoint=(vatt_point_t){
                (slot->obs.left_eye.x+slot->obs.right_eye.x)*.5f,
                (slot->obs.left_eye.y+slot->obs.right_eye.y)*.5f};
            face->eye_velocity_uv_s=(vatt_point_t){
                slot->eye_vx*1000.0f,slot->eye_vy*1000.0f};
            face->status=slot->status;face->gaze_evidence=slot->evidence;
            uint32_t fixation_tip=slot->exiting ? slot->exit_ms : now;
            face->continuous_look_ms=slot->status==VATT_LOOKING_AT_BOT ?
                elapsed(fixation_tip,slot->look_start_ms):0;
            face->cumulative_look_ms=slot->completed_ms+face->continuous_look_ms;
            face->look_count=slot->looks;face->observation=slot->obs;
        }
        if(out->face_count) {
            out->state=VATT_FACE_PRESENT;
            for(int i=0;i<out->face_count;i++)
                if(out->faces[i].status==VATT_LOOKING_AT_BOT) {
                    out->state=VATT_LOOKING_AT_BOT;break;
                }
        } else {
            out->state=t->has_seen_face &&
                elapsed(now,t->last_face_ms)<=t->cfg.no_face_ms ?
                VATT_FACE_PRESENT : VATT_NO_FACE;
        }
    } else out->state=VATT_UNAVAILABLE;
    uint32_t id=camera_ok ? primary(t):0;
    if(id!=t->primary_id) {
        event(out,VATT_PRIMARY_CHANGED,id,now,0);
        t->primary_id=id;
    }
    out->primary_track_id=id;
    return true;
}
