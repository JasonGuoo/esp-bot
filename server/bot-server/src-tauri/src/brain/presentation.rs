//! Freeze semantics before TTS/image preparation; playback has a separate lease.
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use super::scene::Scene;

#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct PresentationConfig {pub max_segments:usize,pub max_segment_chars:usize,pub reading_dwell_ms:u64}
impl Default for PresentationConfig{fn default()->Self{Self{max_segments:16,max_segment_chars:200,reading_dwell_ms:2500}}}
impl PresentationConfig{pub fn validate(&self)->Result<(),String>{
    if !(1..=32).contains(&self.max_segments)||!(40..=500).contains(&self.max_segment_chars)||!(500..=30_000).contains(&self.reading_dwell_ms){return Err("presentation_limits_invalid".into());}Ok(())}}

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct PlanContext {pub household_id:String,pub daily_conversation_id:String,pub conversation_turn_id:String,pub task_id:Option<String>}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct PresentationPlan {pub schema_version:u32,pub presentation_id:String,pub revision:u32,pub digest:String,pub context:PlanContext,pub segments:Vec<PresentationSegment>,pub created_at_ms:i64}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct PresentationSegment {
    pub segment_id:String,pub speech:String,pub caption:String,pub expression:String,
    pub visual:Value,pub visual_required:bool,pub wait_for_first_frame:bool,pub dwell_ms:u64,
    pub fallback:String,pub asset_ids:Vec<String>,pub speech_asset_id:Option<String>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum PlaybackPurpose{WakeAck,TaskAck,Progress,Content,Final,Preview}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct OutputLease {
    pub output_lease_id:String,pub device_id:String,pub activation_id:String,pub runtime:Value,
    pub purpose:PlaybackPurpose,pub expires_at_ms:i64,pub revoked:bool,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackReceipt {
    pub receipt_id:String,pub playback_id:String,pub presentation_id:String,pub presentation_revision:u32,
    pub segment_id:String,pub output_lease_id:String,pub sequence:u64,pub state:String,
    pub device_monotonic_ms:Option<u64>,pub sample_position:Option<u64>,
    #[serde(default)]pub detail:Value,
}

pub struct PresentationPlanner{config:PresentationConfig}
impl Default for PresentationPlanner{fn default()->Self{Self{config:PresentationConfig::default()}}}
impl PresentationPlanner{
    pub fn new(config:PresentationConfig)->Result<Self,String>{config.validate()?;Ok(Self{config})}
    pub fn freeze(&self,context:PlanContext,scene:&Scene,structured:Option<&Value>,visual_required:bool)->Result<PresentationPlan,String>{
        for id in [&context.household_id,&context.daily_conversation_id,&context.conversation_turn_id]{super::artifacts::valid_id(id)?;}
        let visual=structured.and_then(|v|v.get("visual")).cloned().unwrap_or_else(||json!({"kind":"none"}));
        let mut segments=Vec::new();
        if let Some(proposed)=structured.and_then(|v|v.get("presentation")).and_then(|v|v.get("segments")).and_then(Value::as_array){
            if proposed.len()>self.config.max_segments{return Err("presentation_segment_limit".into());}
            for item in proposed{
                let speech=item["speech"].as_str().unwrap_or("").trim();
                if speech.is_empty()||speech.chars().count()>self.config.max_segment_chars{return Err("presentation_speech_limit".into());}
                let expression=item["expression"].as_str().filter(|s|crate::face_catalog_generated::EXPRESSION_IDS.contains(s)).unwrap_or(&scene.expression);
                let local_visual=item.get("visual").cloned().unwrap_or_else(||json!({"kind":"none"}));
                validate_visual(&local_visual)?;
                segments.push(segment(speech,expression,local_visual,item["visual_required"].as_bool().unwrap_or(false),self.config.reading_dwell_ms));
            }
        }
        if segments.is_empty(){
            validate_visual(&visual)?;
            let parts=split_speech(&scene.speech,self.config.max_segment_chars);
            if parts.len()>self.config.max_segments{return Err("presentation_segment_limit".into());}
            for (i,part) in parts.iter().enumerate(){segments.push(segment(part,&scene.expression,if i==0{visual.clone()}else{json!({"kind":"none"})},i==0&&visual_required,self.config.reading_dwell_ms));}
        }
        if segments.is_empty(){return Err("presentation_empty".into());}
        // The hash covers semantic content and source context, before any asset
        // work starts. Asset additions create a new plan revision in the store.
        let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"context":context,"segments":segments})).map_err(|e|e.to_string())?));
        Ok(PresentationPlan{schema_version:1,presentation_id:format!("presentation_{}",uuid::Uuid::new_v4().simple()),revision:1,digest,context,segments,created_at_ms:crate::time::now_ms() as i64})
    }
}

impl PresentationPlan {
    /// A complete first sentence from the frozen text. Never cut a clause or
    /// publish content whose meaning depends on visual delivery.
    pub fn answer_lead(&self) -> Option<&str> {
        let first=self.segments.first()?;
        if first.wait_for_first_frame || first.visual_required || first.visual["kind"]!="none" {return None;}
        let text=first.speech.trim();
        let end=text.char_indices().find(|(_,c)|matches!(c,'。'|'！'|'？'|'!'|'?'|'\n'))
            .map(|(i,c)|i+c.len_utf8());
        // An unpunctuated segment may end only because the planner hit its
        // size limit. The last word alone cannot establish a sentence boundary.
        if end.is_none() && self.segments.len()>1 {return None;}
        let lead=text[..end.unwrap_or(text.len())].trim();
        (!lead.is_empty() && lead.chars().count()<=80).then_some(lead)
    }
}

fn segment(speech:&str,expression:&str,visual:Value,required:bool,dwell:u64)->PresentationSegment{
    let references_visual=["看这里","这张图","图中","红色箭头","look here","in this picture"].iter().any(|s|speech.contains(s));
    PresentationSegment{segment_id:format!("segment_{}",uuid::Uuid::new_v4().simple()),speech:speech.into(),caption:speech.into(),expression:expression.into(),
        wait_for_first_frame:required||references_visual,visual,visual_required:required,dwell_ms:dwell,fallback:"caption_and_speech".into(),asset_ids:Vec::new(),speech_asset_id:None}
}
fn validate_visual(v:&Value)->Result<(),String>{
    if serde_json::to_vec(v).map_err(|e|e.to_string())?.len()>16_384{return Err("presentation_visual_limit".into());}
    if !matches!(v["kind"].as_str().unwrap_or("none"),"none"|"photo"|"bars"|"text"|"spelling"|"stroke"|"diagram"|"media"|"artifact"){return Err("presentation_visual_kind".into());}
    if v.get("html").is_some()||v.get("javascript").is_some(){return Err("presentation_executable_content".into());}Ok(())
}
pub fn split_speech(text:&str,limit:usize)->Vec<String>{
    let mut result=Vec::new();let mut current=String::new();let mut count=0;
    for ch in text.chars().filter(|c|!c.is_control()||*c=='\n'){
        current.push(ch);count+=1;
        if count>=limit||("。！？!?\n".contains(ch)&&count>=limit/3){let s=current.trim();if !s.is_empty(){result.push(s.into());}current.clear();count=0;}
    }
    if !current.trim().is_empty(){result.push(current.trim().into());}result
}

pub fn proposal_schema()->Value{json!({"type":"object","additionalProperties":false,"properties":{"segments":{"type":"array","maxItems":16,"items":{
    "type":"object","properties":{"speech":{"type":"string","maxLength":200},"expression":{"type":"string","enum":crate::face_catalog_generated::EXPRESSION_IDS},
    "visual":{"type":"object"},"visual_required":{"type":"boolean"}},"required":["speech","expression","visual","visual_required"]}}},"required":["segments"]})}

#[cfg(test)]
mod feedback_tests {
    use super::*;

    #[test]
    fn preview_requires_a_complete_independent_sentence() {
        let context=PlanContext{household_id:"home".into(),daily_conversation_id:"day".into(),conversation_turn_id:"turn".into(),task_id:None};
        let scene=Scene{speech:"先说结论。再解释。".into(),expression:"happy".into(),cards:vec![]};
        let mut plan=PresentationPlanner::default().freeze(context,&scene,None,false).unwrap();
        assert_eq!(plan.answer_lead(),Some("先说结论。"));
        plan.segments[0].speech="很".repeat(90)+"。";
        assert_eq!(plan.answer_lead(),None);
        plan.segments[0].speech="第一段还没有讲完".into();
        plan.segments.push(plan.segments[0].clone());
        assert_eq!(plan.answer_lead(),None);
        plan.segments.pop();
        plan.segments[0].speech="看这里。".into();
        plan.segments[0].wait_for_first_frame=true;
        assert_eq!(plan.answer_lead(),None);
        plan.segments[0].wait_for_first_frame=false;
        plan.segments[0].visual=json!({"kind":"diagram"});
        assert_eq!(plan.answer_lead(),None);
    }
}
