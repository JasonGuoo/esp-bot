#include "attention.h"
#include <assert.h>
#include <stdio.h>

static attention_observation_t sight(uint32_t ms, float yaw, float pitch, bool looking) {
    return (attention_observation_t){
        .timestamp_ms=ms, .track_id=42,
        .person_yaw_from_camera_deg=yaw, .person_pitch_from_camera_deg=pitch,
        .person_confidence=.94f, .user_looking_at_robot=looking,
        .looking_confidence=looking ? .93f : .20f
    };
}
static attention_pose_t pose(float yaw, float pitch) {
    return (attention_pose_t){.yaw_deg=yaw, .pitch_deg=pitch};
}

static void test_presence_is_not_contact(void) {
    attention_controller_t c;
    attention_config_t cfg = attention_default_config();
    assert(attention_controller_init(&c,&cfg));
    attention_pose_t head = pose(0,0);
    attention_observation_t obs = sight(100,25,8,false);
    assert(attention_controller_observe(&c,&obs,&head));
    attention_output_t out = attention_controller_step(&c,&head,110,true);
    assert(out.person_tracked && !out.person_looking_at_robot);
    assert(!out.mutual_eye_contact);
    assert(out.eye_x>0 && out.eye_y<0);
    assert(out.eye_x<=75 && out.eye_y>=-75);
    out=attention_controller_step(&c,&head,210,true);
    assert(out.head_motion_requested && out.head_target_yaw_deg>0);
    assert(out.head_target_yaw_deg<=5.51f); /* 55 deg/s, 100 ms */
}
static void test_contact_requires_alignment_and_dwell(void) {
    attention_controller_t c;
    attention_config_t cfg=attention_default_config();
    assert(attention_controller_init(&c,&cfg));
    attention_pose_t head=pose(0,0);
    attention_observation_t obs=sight(100,24,0,true);
    assert(attention_controller_observe(&c,&obs,&head));
    attention_output_t out=attention_controller_step(&c,&head,110,true);
    assert(!out.mutual_eye_contact); /* big angle, must turn head */
    head=pose(20,0);
    obs=sight(210,4,0,true); /* camera turned: target is still 24deg in base */
    assert(attention_controller_observe(&c,&obs,&head));
    out=attention_controller_step(&c,&head,215,true);
    assert(out.eye_x>0 && out.eye_x<75 && !out.mutual_eye_contact);
    for(uint32_t ms=260;ms<=610;ms+=50) {
        obs=sight(ms,4,0,true);
        assert(attention_controller_observe(&c,&obs,&head));
        out=attention_controller_step(&c,&head,ms+5,true);
    }
    assert(out.person_tracked && out.mutual_eye_contact);
    assert(out.residual_yaw_deg==4.0f);
}
static void test_lost_target_and_no_head_motors(void) {
    attention_controller_t c;
    attention_config_t cfg=attention_default_config();
    assert(attention_controller_init(&c,&cfg));
    attention_pose_t head=pose(0,0);
    attention_observation_t obs=sight(100,-16,-4,true);
    assert(attention_controller_observe(&c,&obs,&head));
    attention_output_t out=attention_controller_step(&c,&head,110,false);
    assert(out.eye_x<0 && out.eye_y>0 && !out.head_motion_requested);
    out=attention_controller_step(&c,&head,1000,false);
    assert(!out.person_tracked && !out.mutual_eye_contact);
    assert(out.eye_x==0 && out.eye_y==0);
}
static void test_invalid_and_out_of_order_observations(void) {
    attention_controller_t c;
    attention_config_t cfg=attention_default_config();
    assert(attention_controller_init(&c,&cfg));
    attention_pose_t head=pose(0,0);
    attention_observation_t obs=sight(100,1,0,true);
    obs.person_confidence=.3f;
    assert(!attention_controller_observe(&c,&obs,&head));
    obs.person_confidence=.94f;
    assert(attention_controller_observe(&c,&obs,&head));
    obs.timestamp_ms=99;
    assert(!attention_controller_observe(&c,&obs,&head));
    obs=sight(230,1,0,true);
    obs.track_id=99;
    assert(attention_controller_observe(&c,&obs,&head));
    attention_output_t out=attention_controller_step(&c,&head,240,true);
    assert(!out.mutual_eye_contact);
}
static void test_unreachable_or_uncertain_person(void) {
    attention_controller_t c;
    attention_config_t cfg=attention_default_config();
    assert(attention_controller_init(&c,&cfg));
    attention_pose_t head=pose(0,0);
    attention_observation_t obs=sight(100,89,-29,true);
    assert(attention_controller_observe(&c,&obs,&head));
    attention_output_t out=attention_controller_step(&c,&head,110,true);
    assert(!out.mutual_eye_contact && out.eye_x==75 && out.eye_y==75);
    obs=sight(160,2,0,true);
    obs.looking_confidence=.2f;
    assert(attention_controller_observe(&c,&obs,&head));
    out=attention_controller_step(&c,&head,165,false);
    assert(!out.person_looking_at_robot && !out.mutual_eye_contact);
}
int main(void) {
    test_presence_is_not_contact();
    test_contact_requires_alignment_and_dwell();
    test_lost_target_and_no_head_motors();
    test_invalid_and_out_of_order_observations();
    test_unreachable_or_uncertain_person();
    puts("attention coordinator: all native C tests passed");
    return 0;
}
