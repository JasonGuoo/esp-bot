#include "vision_attention.h"
#include <math.h>
#include <string.h>

/* Bounded grayscale eye ROI analysis. This deliberately returns candidate
 * iris/pupil locations, NOT a trained iris segmentation nor gaze angles.
 * Inter-eye landmarks are from the original full-resolution image.
 */
static float clampf(float x,float a,float b) {
    return x<a?a:(x>b?b:x);
}
static uint8_t sample(const vatt_gray8_t *im,float px,float py) {
    int x=(int)(px+.5f),y=(int)(py+.5f);
    if (x < 0) x = 0;
    if (y < 0) y = 0;
    if (x >= im->width) x = im->width - 1;
    if (y >= im->height) y = im->height - 1;
    return im->data[(size_t)y*im->stride+(size_t)x];
}
bool vatt_pupil_candidate(const vatt_gray8_t *im,
                          vatt_point_t eye,vatt_point_t other,
                          vatt_pupil_t *out) {
    if(!out) return false;
    memset(out,0,sizeof(*out));
    if(!im || !im->data || im->width < 32 || im->height < 24 ||
       im->stride < im->width || !isfinite(eye.x) || !isfinite(eye.y) ||
       !isfinite(other.x) || !isfinite(other.y)) return false;
    float dx=other.x-eye.x,dy=other.y-eye.y;
    float ipd=sqrtf(dx*dx+dy*dy);
    /* For smaller faces the pupil often spans too few pixels to infer. */
    if(ipd<40.0f || ipd>im->width*.8f) return false;
    float ux=dx/ipd,uy=dy/ipd;
    if(ux<0) {ux=-ux;uy=-uy;}  /* standardized +u points image-right */
    float vx=-uy,vy=ux;
    float half_w=clampf(ipd*.21f,7.5f,32.0f);
    float half_h=clampf(ipd*.12f,4.5f,20.0f);
    /* No direct evidence the full eye ROI lies within sensor bounds. */
    if(eye.x<half_w*1.7f || eye.x>im->width-half_w*1.7f ||
       eye.y<half_h*2.1f || eye.y>im->height-half_h*2.1f) return false;
    float radius=clampf(ipd*.045f,2.0f,5.0f);
    float max_score=-1000, best_u=0,best_v=0,best_q=0;
    float max_u=half_w*.73f,max_v=half_h*.54f;
    /* Evaluate local 3x3 core darkness versus a ring of surrounding pixels,
       preferring compact dark objects near the eye landmark over eyebrows. */
    for (float py=-max_v;py<=max_v;py+=1.4f) {
        for(float px=-max_u;px<=max_u;px+=1.4f) {
            float x=eye.x+px*ux+py*vx,y=eye.y+px*uy+py*vy;
            float core=0,ring=0;
            for(int j=-1;j<=1;j++) for(int i=-1;i<=1;i++)
                core+=sample(im,x+i,y+j);
            core/=9.0f;
            static const float offsets[8][2]={
                {1,0},{-1,0},{0,1},{0,-1},
                {.7071f,.7071f},{-.7071f,.7071f},
                {.7071f,-.7071f},{-.7071f,-.7071f}
            };
            for(int k=0;k<8;k++) {
                float ox=offsets[k][0]*radius*1.85f;
                float oy=offsets[k][1]*radius*1.85f;
                ring+=sample(im,x+ox,y+oy);
            }
            ring/=8.0f;
            float contrast=ring-core;
            float cdist=(px*px)/(half_w*half_w) +
                        (py*py)/(half_h*half_h);
            float score=contrast-4.0f*cdist;
            if(score>max_score) {
                max_score=score;best_u=px;best_v=py;
                best_q=contrast;
            }
        }
    }
    /* Conservative confidence gate. Dark shadow/eyelash can still pass;
       consumers must pair eyes and use independent calibrated gaze logic. */
    if(best_q<12.0f || max_score<8.0f) return false;
    out->valid=true;
    out->center_px=(vatt_point_t){
        eye.x+best_u*ux+best_v*vx,
        eye.y+best_u*uy+best_v*vy};
    out->eye_relative_u=clampf(best_u/half_w,-1,1);
    out->eye_relative_v=clampf(best_v/half_h,-1,1);
    out->quality=clampf((best_q-8.0f)/42.0f,0,1);
    return true;
}
