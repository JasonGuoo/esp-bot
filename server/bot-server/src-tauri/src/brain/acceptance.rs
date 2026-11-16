//! Manual acceptance records. Creating a report never marks a scenario passed.
use std::path::Path;
use serde_json::{json,Value};
use super::{settings,valid_turn_id};

pub fn scenarios()->Value{
    json!([
        {"id":"wake","issues":[19,27],"action":"安静、噪声、播放中分别说完整的小呆小呆","expected":"只显式唤醒上传；ACK恢复输入；AEC关闭时报告半双工"},
        {"id":"parallel_audio","issues":[16,26],"action":"同时测量ASR和声纹；关闭声纹模型再问同一句话","expected":"ASR不等待身份；短音频/重叠保留unknown；候选不授权"},
        {"id":"household_day","issues":[15,17],"action":"两名成员分两次唤醒，追问同一件事，再重启Mac","expected":"共享家庭当天主题；activation和native session独立；重启不重放副作用"},
        {"id":"compact","issues":[9,17],"action":"连续对话超过8轮、改变检索命中，手动触发compact","expected":"普通召回排序不删会话；compact完成消息确认后才推进watermark"},
        {"id":"task_concurrency","issues":[18],"action":"开始长资料任务，同时进行另一轮简短对话","expected":"独立工作会话运行，交互不占据全局锁，任务事件能恢复"},
        {"id":"progress","issues":[19,11],"action":"分别让ASR、搜索、图像、TTS等待较长时间","expected":"提示对应实际阶段，task ACK/progress不打开下一轮麦克风"},
        {"id":"segments","issues":[20,21],"action":"请求拼写、分数图解、汉字或带字幕的短媒体","expected":"冻结语义一致；首帧后旁白；每段真实播放回执；缺笔顺数据诚实降级"},
        {"id":"barge_in","issues":[18,19,27],"action":"回答播放中点击停止或在已通过AEC自检的模式下说话","expected":"旧lease及剩余段失效；新轮不接收旧内容；不把停止播报当撤销打印"},
        {"id":"reconnect","issues":[14,18,20],"action":"播放中断网、恢复连接，并重复发送旧receipt","expected":"旧runtime/lease拒绝，receipt幂等，连接恢复不重播旧副作用"},
        {"id":"source_correction","issues":[22,23,24],"action":"在时间线把候选说话人改为另一人，再改正转写","expected":"显示修订和引用；提到谁与谁在说分开；旧派生版本失效并重建"},
        {"id":"forget","issues":[15,22,24],"action":"删除一天或一个来源，再重启并搜索","expected":"原文、派生记忆、图索引与结果内容均不可召回；失败清理有待重试状态"},
        {"id":"learning","issues":[23],"action":"提问、提示作答、独立答对和隔天复现同一知识点","expected":"证据分introduced/self_reported/correct_with_help/correct_independently/retained_later；不凭一次回答宣称掌握"},
        {"id":"print","issues":[25],"action":"显式请求打印PDF，观察排队、取消、离线及断电恢复","expected":"仅有真实job_id/IPP状态才报告；提交未知不自动重打；预览可重开"},
        {"id":"stop_console","issues":[7],"action":"只使用设备发起一轮，刷新Mac界面后点停止","expected":"按钮由服务状态恢复并停止对应device session；已完成旧结果不让按钮常亮"},
        {"id":"route_cooldown","issues":[8],"action":"让首选路由出现网络或上游错误后发起新轮","expected":"冷却期间跳过该路由；429原策略保留；已产生输出不重试副作用"},
        {"id":"settings_close","issues":[10],"action":"修改唤醒设置后立即离开页面，连续快速重复","expected":"最后有效编辑随关闭快照保存，Stop仍优先，旧页面事件不能改新页面"},
        {"id":"privacy","issues":[15,22,24],"action":"带旧个人记忆迁移，关闭家庭云召回和归档，查看请求与磁盘","expected":"不把旧私有内容自动分享；模型/声纹录音不上传；缺源内容不伪造记忆"},
        {"id":"resource_budget","issues":[21,27,28],"action":"记录Mac内存/CPU、ESP最小堆和一小时连续交互；模拟无模型/无FFmpeg/无打印机","expected":"并发/队列/字节有界，缺依赖明确显示，不下载、不崩溃、不假报通过"}
    ])
}

pub fn dispatch(root:&Path,method:&str,payload:&Value)->Result<Value,String>{
    let directory=root.join("runtime/acceptance");settings::private_dir(&directory)?;
    if method=="acceptance.scenarios"{return Ok(scenarios());}
    if method=="acceptance.create"{
        let id=format!("acceptance-{}",uuid::Uuid::new_v4().simple());
        let items=scenarios().as_array().ok_or("acceptance_catalog")?.iter().map(|item|{
            let mut item=item.clone();item["status"]=json!("not_run");item["observation"]=json!("");item
        }).collect::<Vec<_>>();
        let report=json!({"schema_version":1,"report_id":id,"created_at_ms":crate::time::now_ms(),
            "application_version":env!("CARGO_PKG_VERSION"),"opencode_source_version":crate::harness::SOURCE_VERSION,
            "environment":payload.get("environment").cloned().unwrap_or(json!({})),"scenarios":items,"automated_run":false});
        settings::private_write(&directory.join(format!("{id}.json")),&serde_json::to_vec_pretty(&report).map_err(|e|e.to_string())?)?;
        return Ok(report);
    }
    let id=payload["report_id"].as_str().filter(|id|valid_turn_id(id)&&id.starts_with("acceptance-")).ok_or("acceptance_report_id")?;
    let path=directory.join(format!("{id}.json"));let bytes=std::fs::read(&path).map_err(|_|"acceptance_report_missing")?;
    if bytes.len()>262144{return Err("acceptance_report_limit".into());}
    let mut report:Value=serde_json::from_slice(&bytes).map_err(|_|"acceptance_report_invalid")?;
    if method=="acceptance.record"{
        let status=payload["status"].as_str().filter(|s|matches!(*s,"not_run"|"passed"|"failed"|"blocked")).ok_or("acceptance_status")?;
        let observation=payload["observation"].as_str().filter(|s|s.chars().count()<=4000).ok_or("acceptance_observation")?;
        if status=="passed"&&observation.trim().is_empty(){return Err("acceptance_observation_required".into());}
        let item=report["scenarios"].as_array_mut().ok_or("acceptance_report_invalid")?.iter_mut().find(|s|s["id"]==payload["scenario_id"]).ok_or("acceptance_scenario_missing")?;
        item["status"]=json!(status);item["observation"]=json!(observation);item["recorded_at_ms"]=json!(crate::time::now_ms());
        settings::private_write(&path,&serde_json::to_vec_pretty(&report).map_err(|e|e.to_string())?)?;
    }else if method!="acceptance.get"{return Err("acceptance_method_unsupported".into());}
    Ok(report)
}
