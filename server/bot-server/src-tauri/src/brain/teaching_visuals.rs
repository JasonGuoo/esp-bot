//! Small reviewed local templates for the fast answer path. Inputs come from the
//! current question, never from generated executable code or inferred numeric data.
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
const SVM_SCENE:&str=include_str!("../../../local-models/svm_radial_v1.json");

pub(super) fn append(reply:&mut Value,question:&str,preference:&str,limit:usize)->Option<String> {
    let parts=reply.get_mut("presentation")?.get_mut("segments")?.as_array_mut()?;
    if parts.len()>=limit || parts.iter().any(|part|part["visual"]["template"].is_object()) {return None;}
    let additions=if svm_requested(question,preference) {
        let scene:Value=serde_json::from_str(SVM_SCENE).ok()?;
        let digest=format!("{:x}",Sha256::digest(SVM_SCENE.as_bytes()));
        scene["steps"].as_array()?.iter().map(|step|json!({"speech":step["speech"],"expression":"curious","visual_required":true,
            "visual":{"kind":"diagram","requested":false,"title":step["title"],"body":"固定教学示例；不是 SVM 训练结果。",
                "diagram":{"type":"svm_radial","stage":step["stage"],"scene_digest":digest},
                "template":{"id":"svm_radial","version":1,"source":"reviewed_example","digest":digest}}})).collect::<Vec<_>>()
    } else {vec![supplement(question,preference)?]};
    // A frozen teaching example is all-or-nothing; never omit the caveat or
    // silently present only half the mapping when the segment budget is full.
    if parts.len()+additions.len()>limit {return None;}
    let speech=additions.iter().filter_map(|part|part["speech"].as_str()).collect::<Vec<_>>().join("");
    parts.extend(additions);
    Some(speech)
}

fn allowed(question:&str,preference:&str)->bool {
    preference!="off" && question.chars().count()<=600
        && !["不要图","不用图","别画","不画图","只用文字","只说","只讲","不要视频","不要动画"].iter().any(|s|question.contains(s))
        && (preference!="auto" || ["图","画","展示","演示"].iter().any(|s|question.contains(s)))
}
fn svm_requested(question:&str,preference:&str)->bool {
    let lower=question.to_lowercase();
    allowed(question,preference) && (lower.contains("svm") || lower.contains("支持向量机"))
        && ["解释","讲解","举例","为什么","映射","核技巧","核方法","演示","图解","看不懂"].iter().any(|s|lower.contains(s))
        && !["我的数据","这些数据","帮我训练","运行训练"].iter().any(|s|question.contains(s))
}

pub(super) fn supplement(question:&str,preference:&str)->Option<Value> {
    let text:String=question.chars().filter(|c|!c.is_whitespace()).collect();
    if !allowed(&text,preference) {return None;}
    if !["解释","说明","理解","怎么","什么","为什么","看懂","图","画","展示","演示","教","讲讲","分数","数轴"].iter().any(|s|text.contains(s)) {return None;}
    if let Some((numerator,denominator))=fraction(&text) {
        return Some(json!({"speech":format!("把整体平均分成{denominator}份，涂色的{numerator}份表示{numerator}/{denominator}。"),
            "expression":"curious","visual_required":true,"visual":{"kind":"diagram","requested":false,
                "title":format!("{numerator}/{denominator} 是多少"),"body":"每一格大小相同，涂色部分表示分子。",
                "diagram":{"type":"fraction","numerator":numerator,"denominator":denominator},
                "template":{"id":"fraction","version":1,"source":"current_question"}}}));
    }
    if text.contains("数轴") {
        let values=numbers(&text);
        let (low,high)=match values.as_slice(){[] if !text.chars().any(|c|"零一二两三四五六七八九十百千万负".contains(c))=>(-3,3),
            [low,high] if *low>=-20 && *high<=20 && low<high && high-low<=10=>(*low,*high),_=>return None};
        return Some(json!({"speech":format!("这条示例数轴从{low}到{high}，每个相邻刻度之间相差一。"),
            "expression":"curious","visual_required":true,"visual":{"kind":"diagram","requested":false,
                "title":"数轴上的位置","body":"向右越来越大，向左越来越小。",
                "diagram":{"type":"number_line","start":low,"end":high,"ticks":high-low+1},
                "template":{"id":"number_line","version":1,"source":"current_question"}}}));
    }
    None
}

fn numbers(text:&str)->Vec<i32>{
    text.split(|c:char|!c.is_ascii_digit() && c!='-').filter(|s|!s.is_empty())
        .map(|s|s.parse().unwrap_or(i32::MAX)).take(8).collect()
}

fn fraction(text:&str)->Option<(u32,u32)>{
    let chars:Vec<char>=text.chars().collect();
    let mut found=Vec::new();
    for (i,ch) in chars.iter().enumerate(){
        if *ch=='/' && text.contains("分数") {
            let left:String=chars[..i].iter().rev().take_while(|c|c.is_ascii_digit()).copied().collect::<Vec<_>>().into_iter().rev().collect();
            let right:String=chars[i+1..].iter().take_while(|c|c.is_ascii_digit()).collect();
            if i>left.len() && matches!(chars[i-left.len()-1],'-'|'.') || chars.get(i+1+right.len())==Some(&'.') {return None;}
            if let (Ok(n),Ok(d))=(left.parse::<u32>(),right.parse::<u32>()){found.push((n,d));}
        }
    }
    if text.matches("分之").count()==1 {
        let (left,right)=text.split_once("分之")?;
        let digit=|c|match c {'零'=>Some(0),'一'=>Some(1),'二'|'两'=>Some(2),'三'=>Some(3),'四'=>Some(4),'五'=>Some(5),'六'=>Some(6),'七'=>Some(7),'八'=>Some(8),'九'=>Some(9),'十'=>Some(10),_=>None};
        if let (Some(d),Some(n))=(left.chars().last().and_then(digit),right.chars().next().and_then(digit)) {
            // Compound numbers require a richer parser; never turn 十三分之一 into 1/3.
            let left=left.chars().rev().nth(1);let right=right.chars().nth(1);
            let compound=|c|"零一二两三四五六七八九十百千万亿负".contains(c);
            if left.is_some_and(compound) || right.is_some_and(compound) {return None;}
            found.push((n,d));
        }
    }
    if found.len()!=1 {return None;}
    let (n,d)=found[0];
    (d>0 && d<=12 && n<=d).then_some((n,d))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_visual_is_appended_without_rewriting_frozen_speech_and_keeps_the_frame_gate() {
        let first=json!({"speech":"这是把一个整体平均分开的办法。","expression":"happy","visual":{"kind":"none"},"visual_required":false});
        let mut reply=json!({"speech":first["speech"],"presentation":{"segments":[first.clone()]}});
        assert!(append(&mut reply,"解释三分之一","prefer",16).is_some());
        assert_eq!(reply["presentation"]["segments"][0],first);
        assert!(append(&mut reply,"解释三分之一","prefer",16).is_none(),"one template per frozen reply");
        let scene=super::super::scene::normalize("",Some(&reply),200,&[]);
        let context=super::super::presentation::PlanContext{household_id:"house".into(),daily_conversation_id:"day".into(),conversation_turn_id:"turn".into(),task_id:None};
        let plan=super::super::presentation::PresentationPlanner::default().freeze(context,&scene,Some(&reply),false).unwrap();
        assert_eq!(plan.answer_lead(),first["speech"].as_str());
        assert!(!plan.segments[0].wait_for_first_frame);
        assert!(plan.segments[1].wait_for_first_frame);
        let mut full=json!({"presentation":{"segments":[first]}});
        assert!(append(&mut full,"解释三分之一","prefer",1).is_none());
    }
    #[test]
    fn local_templates_use_exact_current_parameters_and_respect_opt_out() {
        for (question,n,d) in [("解释一下三分之一",1,3),("分数 2/5 是什么意思",2,5)] {
            let v=supplement(question,"prefer").unwrap();
            assert_eq!(v["visual"]["diagram"]["numerator"],n);assert_eq!(v["visual"]["diagram"]["denominator"],d);
            assert_eq!(v["visual_required"],true);
        }
        for question in ["解释十三分之一","解释三分之一百","解释负三分之一","比较分数1/3与2/5","解释分数1/0","说明分数5/3","分数-1/3","分数0.1/3","今天7/8是什么日子","讲个故事","只用文字解释三分之一","数轴从-99999999到9999999999999","数轴从负二到三","我吃了三分之一的蛋糕"] {
            assert!(supplement(question,"prefer").is_none(),"{question}");
        }
        assert!(supplement("解释三分之一","off").is_none());
        assert!(supplement("解释三分之一","auto").is_none());
        assert!(supplement("画三分之一","auto").is_some());
        assert_eq!(supplement("解释从-2到3的数轴","prefer").unwrap()["visual"]["diagram"]["ticks"],6);
    }
    #[test]
    fn the_svm_example_keeps_point_identity_and_freezes_all_four_frame_gates(){
        let scene:Value=serde_json::from_str(SVM_SCENE).unwrap();let plane=scene["plane_z"].as_f64().unwrap();
        for p in scene["points"].as_array().unwrap(){let x=p["x"].as_f64().unwrap();let y=p["y"].as_f64().unwrap();let z=p["z"].as_f64().unwrap();
            assert_eq!(z,x*x+y*y);assert!((z-plane)*p["label"].as_f64().unwrap()>0.0);}
        let first=json!({"speech":"我们用一个固定例子来看。","visual":{"kind":"none"}});
        let mut reply=json!({"presentation":{"segments":[first.clone()]}});
        assert!(append(&mut reply,"解释 SVM 的特征映射","prefer",4).is_none());
        assert!(append(&mut reply,"解释 SVM 的特征映射","prefer",5).is_some());
        let parts=reply["presentation"]["segments"].as_array().unwrap();assert_eq!(parts.len(),5);assert_eq!(parts[0],first);
        assert!(parts[1..].iter().all(|part|part["visual_required"]==true));
        assert!(parts[3]["speech"].as_str().unwrap().contains("没有执行支持向量机训练"));
        assert!(parts[1..].windows(2).all(|p|p[0]["visual"]["template"]["digest"]==p[1]["visual"]["template"]["digest"]));
        for question in ["只用文字解释 SVM","给我的数据做 SVM 映射","帮我训练 SVM"]{assert!(!svm_requested(question,"prefer"));}
    }
}
