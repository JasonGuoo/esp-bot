#include "vision_attention.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

static vatt_observation_t person(float x,bool looking,bool valid) {
    vatt_observation_t o={0};
    o.bbox=(vatt_box_t){x-.09f,.2f,x+.09f,.6f};
    o.left_eye=(vatt_point_t){x-.03f,.32f};
    o.right_eye=(vatt_point_t){x+.03f,.32f};
    o.face_confidence=.94f;
    o.eye_contact_score=looking?.95f:.15f;
    o.gaze_quality=.9f;
    o.eye_contact_valid=valid;
    return o;
}
static vatt_face_t *lookup(vatt_snapshot_t *s,uint32_t id) {
    for(unsigned i=0;i<s->face_count;i++)
        if(s->faces[i].track_id==id) return &s->faces[i];
    return NULL;
}
static void test_multiface_and_independent_fixations(void) {
    vatt_tracker_t *t=vatt_tracker_create(NULL);
    assert(t);
    vatt_snapshot_t s;
    vatt_observation_t obs[2]={person(.22f,true,true),person(.75f,false,true)};
    assert(vatt_tracker_update(t,NULL,0,10,true,&s) && s.state==VATT_NO_FACE);
    assert(vatt_tracker_update(t,obs,2,100,true,&s) && s.face_count==0);
    assert(vatt_tracker_update(t,obs,2,200,true,&s) && s.face_count==2);
    uint32_t id_a=s.faces[0].track_id,id_b=s.faces[1].track_id;
    assert(id_a && id_b && id_a!=id_b);
    assert(s.state==VATT_FACE_PRESENT);
    for(uint32_t ms=300;ms<=500;ms+=100)
        assert(vatt_tracker_update(t,obs,2,ms,true,&s));
    assert(vatt_tracker_update(t,obs,2,580,true,&s));
    assert(s.state==VATT_LOOKING_AT_BOT);
    assert(lookup(&s,id_a)->status==VATT_LOOKING_AT_BOT);
    assert(lookup(&s,id_b)->status==VATT_FACE_PRESENT);
    assert(lookup(&s,id_a)->continuous_look_ms>=350);
    obs[1].eye_contact_score=.95f;
    assert(vatt_tracker_update(t,obs,2,650,true,&s));
    for(uint32_t ms=750;ms<=950;ms+=100)
        assert(vatt_tracker_update(t,obs,2,ms,true,&s));
    assert(vatt_tracker_update(t,obs,2,1060,true,&s));
    assert(lookup(&s,id_b)->status==VATT_LOOKING_AT_BOT);
    assert(lookup(&s,id_b)->continuous_look_ms>=350);
    uint64_t prior_b=lookup(&s,id_b)->cumulative_look_ms;
    obs[0].eye_contact_score=.1f;
    assert(vatt_tracker_update(t,obs,2,1100,true,&s));
    assert(vatt_tracker_update(t,obs,2,1300,true,&s));
    assert(vatt_tracker_update(t,obs,2,1500,true,&s));
    assert(vatt_tracker_update(t,obs,2,1600,true,&s));
    assert(lookup(&s,id_a)->status==VATT_FACE_PRESENT);
    assert(lookup(&s,id_a)->continuous_look_ms==0);
    assert(lookup(&s,id_a)->cumulative_look_ms>350);
    assert(lookup(&s,id_b)->status==VATT_LOOKING_AT_BOT);
    assert(lookup(&s,id_b)->cumulative_look_ms>prior_b);
    assert(vatt_tracker_update(t,NULL,0,2200,true,&s));
    assert(s.state==VATT_NO_FACE);
    assert(vatt_tracker_update(t,NULL,0,2300,false,&s));
    assert(s.state==VATT_UNAVAILABLE);
    assert(!vatt_tracker_update(t,obs,2,1000,true,&s));
    vatt_tracker_destroy(t);
}
static void test_face_alone_never_proves_gaze(void) {
    vatt_tracker_t *t=vatt_tracker_create(NULL);assert(t);
    vatt_snapshot_t s;
    vatt_observation_t p=person(.5f,true,false);
    for(uint32_t ms=100;ms<=900;ms+=100)
        assert(vatt_tracker_update(t,&p,1,ms,true,&s));
    assert(s.face_count==1 && s.state==VATT_FACE_PRESENT);
    assert(s.faces[0].gaze_evidence==VATT_GAZE_UNKNOWN);
    assert(s.faces[0].look_count==0);
    vatt_tracker_destroy(t);
}
static void test_blink_grace_and_separate_people(void) {
    vatt_tracker_t *t=vatt_tracker_create(NULL);assert(t);
    vatt_snapshot_t s;
    vatt_observation_t p=person(.5f,true,true);
    for(uint32_t ms=100;ms<=600;ms+=100)
        assert(vatt_tracker_update(t,&p,1,ms,true,&s));
    assert(s.faces[0].status==VATT_LOOKING_AT_BOT);
    p.eye_contact_valid=false;
    assert(vatt_tracker_update(t,&p,1,700,true,&s));
    assert(s.faces[0].status==VATT_LOOKING_AT_BOT);
    p.eye_contact_valid=true;
    assert(vatt_tracker_update(t,&p,1,800,true,&s));
    assert(s.faces[0].status==VATT_LOOKING_AT_BOT);
    p.eye_contact_valid=false;
    assert(vatt_tracker_update(t,&p,1,1000,true,&s));
    assert(s.faces[0].status==VATT_LOOKING_AT_BOT);
    assert(vatt_tracker_update(t,&p,1,1200,true,&s));
    assert(s.faces[0].status==VATT_FACE_PRESENT);
    assert(s.faces[0].cumulative_look_ms>350);
    vatt_tracker_destroy(t);
}
static void test_pupil_candidates(void) {
    enum {W=160,H=100};
    static uint8_t pixels[W*H];
    memset(pixels,170,sizeof(pixels));
    vatt_gray8_t frame={pixels,W,H,W};
    vatt_pupil_t pupil;
    vatt_point_t eye={55,50},other={105,50};
    assert(!vatt_pupil_candidate(&frame,eye,other,&pupil));
    for(int y=46;y<=54;y++) for(int x=46;x<=54;x++) {
        float dx=(float)x-50,dy=(float)y-50;
        if(dx*dx+dy*dy<=16) pixels[y*W+x]=25;
    }
    assert(vatt_pupil_candidate(&frame,eye,other,&pupil));
    assert(pupil.valid && pupil.quality>0);
    assert(fabsf(pupil.center_px.x-50)<5);
    assert(pupil.eye_relative_u<0);
    eye=(vatt_point_t){10,50};
    assert(!vatt_pupil_candidate(&frame,eye,other,&pupil));
}
int main(void) {
    test_multiface_and_independent_fixations();
    test_face_alone_never_proves_gaze();
    test_blink_grace_and_separate_people();
    test_pupil_candidates();
    puts("vision_attention: multi-person, durations and pupil candidate tests passed");
    return 0;
}
