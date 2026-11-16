//! Prepare bounded device images from evidence produced by the current turn.
use std::{io::Read, net::IpAddr, path::{Path, PathBuf}, sync::Arc, time::Duration};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::harness::ToolEvidence;
use super::worker::ModelWorker;

const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 65536;

pub(super) fn image_required(visual: Option<&Value>, outcome: &str, effect: &str) -> bool {
    let Some(visual) = visual else { return false };
    // Older model replies can omit the intent flag. A failed extra illustration
    // for a direct answer should not turn that answer into an image error.
    visual["requested"].as_bool().unwrap_or_else(|| {
        visual["kind"].as_str().is_some_and(|kind| kind != "none")
            && (outcome != "answered" || effect != "none")
    })
}

pub(crate) fn confirmed_effect(effect: &str, evidence: &[ToolEvidence]) -> bool {
    evidence.iter().any(|record| match effect {
        "read"=>matches!(record.name.as_str(),"read"|"glob"|"grep"|"webfetch")
            || record.name.starts_with("browser_") || record.name.starts_with("deepseek-search_"),
        "write"=>matches!(record.name.as_str(),"write"|"edit"|"patch"|"shell"),
        "other"=>true,
        _=>false,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedImage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub digest: String,
    pub title: String,
}

fn resolve_file(root: &Path, path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.len() > 2048 { return None; }
    let root = root.canonicalize().ok()?;
    let candidate = root.join(path).canonicalize().ok()?;
    (candidate.starts_with(&root) && candidate.is_file()).then_some(candidate)
}

fn verified_file(root: &Path, source: &str, evidence: &[ToolEvidence]) -> Result<Vec<u8>, String> {
    let candidate = resolve_file(root, source).ok_or("image_source_invalid")?;
    let confirmed = evidence.iter().any(|record| {
        if record.name == "glob" {
            record.output.lines().any(|line| resolve_file(root,line.trim()) == Some(candidate.clone()))
        } else if record.name == "read" {
            ["filePath","file_path","path"].iter().any(|field|
                record.input[*field].as_str().and_then(|path|resolve_file(root,path)) == Some(candidate.clone()))
        } else { false }
    });
    if !confirmed { return Err("image_source_unverified".into()); }
    let file = std::fs::File::open(candidate).map_err(|_|"image_open_failed")?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes).map_err(|_|"image_read_failed")?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_INPUT_BYTES { return Err("image_input_size".into()); }
    Ok(bytes)
}

pub(super) fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a,b,c,_]=ip.octets();
            !ip.is_private() && !ip.is_loopback() && !ip.is_link_local() && !ip.is_multicast() &&
                !ip.is_broadcast() && a!=0 && a<224 && !(a==100 && (64..=127).contains(&b)) &&
                !(a==192 && b==0 && c==0) && !(a==198 && (b==18 || b==19))
        },
        IpAddr::V6(ip) => {
            if let Some(mapped)=ip.to_ipv4_mapped() {return public_address(IpAddr::V4(mapped));}
            let first=ip.segments()[0];
            (0x2000..=0x3fff).contains(&first) && !ip.is_multicast()
        },
    }
}

pub(super) fn allowed_url(url:&reqwest::Url, hosts:&[String]) -> bool {
    let Some(host)=url.host_str() else {return false};
    url.scheme()=="https" && url.username().is_empty() && url.password().is_none()
        && url.port_or_known_default()==Some(443) && host.parse::<IpAddr>().is_err()
        && !host.eq_ignore_ascii_case("localhost") && !host.ends_with(".local")
        && hosts.iter().any(|allowed|allowed.eq_ignore_ascii_case(host))
}

async fn verified_remote(source:&str, hosts:&[String], evidence:&[ToolEvidence]) -> Result<Vec<u8>,String> {
    let confirmed=evidence.iter().filter(|record|record.name=="webfetch" ||
        record.name.starts_with("browser_") || record.name.starts_with("deepseek-search_")).any(|record| {
        record.input["url"].as_str()==Some(source) || record.output.contains(source)
    });
    if !confirmed {return Err("image_source_unverified".into());}
    let mut url=reqwest::Url::parse(source).map_err(|_|"image_source_invalid")?;
    for _ in 0..=3 {
        if !allowed_url(&url,hosts) {return Err("image_host_not_allowed".into());}
        let host=url.host_str().ok_or("image_host_invalid")?.to_owned();
        let addresses=tokio::net::lookup_host((host.as_str(),443)).await
            .map_err(|_|"image_dns_failed")?.collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|addr|!public_address(addr.ip())) {
            return Err("image_address_invalid".into());
        }
        let client=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5)).resolve_to_addrs(&host,&addresses).build().map_err(|_|"image_client_failed")?;
        let mut response=client.get(url.clone()).send().await.map_err(|_|"image_download_failed")?;
        if response.status().is_redirection() {
            let location=response.headers().get(reqwest::header::LOCATION).and_then(|v|v.to_str().ok()).ok_or("image_redirect_invalid")?;
            url=url.join(location).map_err(|_|"image_redirect_invalid")?;
            continue;
        }
        if !response.status().is_success() || response.content_length().is_some_and(|size|size>MAX_INPUT_BYTES) {
            return Err("image_download_failed".into());
        }
        let mut bytes=Vec::new();
        while let Some(chunk)=response.chunk().await.map_err(|_|"image_download_failed")? {
            if bytes.len()+chunk.len()>MAX_INPUT_BYTES as usize {return Err("image_input_size".into());}
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {return Err("image_input_size".into());}
        return Ok(bytes);
    }
    Err("image_redirect_limit".into())
}

fn line_contains_value(line: &str, value: f64) -> bool {
    line.split(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f64>().ok())
        .any(|candidate| candidate == value)
}

fn chart_request(visual: &Value, evidence: &[ToolEvidence], transcript: &str) -> Result<(Value,String),String> {
    let unit=visual["unit"].as_str().filter(|s|!s.is_empty() && s.chars().count()<=12).ok_or("chart_unit_missing")?;
    let items=visual["series"].as_array().filter(|items|items.len()==2).ok_or("chart_series_invalid")?;
    let mut series=Vec::with_capacity(2);
    for item in items {
        let label=item["label"].as_str().filter(|s|!s.is_empty() && s.chars().count()<=12).ok_or("chart_label_invalid")?;
        let value=item["value"].as_f64().filter(|v|v.is_finite() && *v>=0.0 && *v<=1_000_000.0).ok_or("chart_value_invalid")?;
        series.push((label.to_owned(),value));
    }
    let confirmed=evidence.iter().filter(|record|record.name=="read" || record.name=="grep").any(|record| {
        (record.output.contains(unit) || transcript.contains(unit)) &&
            series.iter().all(|(label,value)|record.output.lines().any(|line|line.contains(label) && line_contains_value(line,*value)))
    });
    if !confirmed { return Err("chart_data_unverified".into()); }
    let (first,second)=(&series[0],&series[1]);
    let conclusion=if first.1==second.1 {
        format!("{}和{}都是 {}{}，一样多。",first.0,second.0,first.1,unit)
    } else {
        let (high,low)=if first.1>second.1 {(first,second)} else {(second,first)};
        format!("{}是 {}{}，{}是 {}{}。{}多 {}{}。",high.0,high.1,unit,low.0,low.1,unit,high.0,high.1-low.1,unit)
    };
    Ok((json!({"kind":"bars","unit":unit,"series":[
        {"label":first.0,"value":first.1},{"label":second.0,"value":second.1}
    ]}),conclusion))
}

/// Decide any deterministic speech changes before the presentation is frozen.
/// Asset preparation below must never rewrite this semantic conclusion.
pub(crate) fn semantic_speech(visual:Option<&Value>,evidence:&[ToolEvidence],transcript:&str)->Result<Option<String>,String>{
    match visual.and_then(|v|v["kind"].as_str()){
        Some("bars")=>chart_request(visual.unwrap(),evidence,transcript).map(|(_,speech)|Some(speech)),
        _=>Ok(None),
    }
}

/// Load and pin real stroke-source evidence before any speech or image starts.
/// Only a host-configured local dataset can supply it; model-proposed SVG paths
/// or median coordinates are never accepted as stroke-order evidence.
pub(crate) async fn semantic_speech_with_tasks(tasks:&Arc<super::tasks::TaskService>,visual:Option<&mut Value>,evidence:&[ToolEvidence],transcript:&str)->Result<Option<String>,String>{
    let Some(visual)=visual else{return Ok(None)};
    if visual["kind"]!="stroke"{return semantic_speech(Some(visual),evidence,transcript);}
    let character=visual["character"].as_str().filter(|s|s.chars().count()==1).ok_or("stroke_character_invalid")?.to_owned();
    if let Some(object)=visual.as_object_mut(){object.remove("_stroke_record_sha256");object.remove("_stroke_unavailable_reason");}
    let info=super::capabilities::stroke_info(tasks,&character).await.unwrap_or_else(|_|json!({"visual_available":false,"unavailable_reason":"stroke_dataset_unavailable"}));
    if info["visual_available"]==true{
        let count=info["stroke_count"].as_u64().filter(|n|*n>0&&*n<=48).ok_or("stroke_count_invalid")?;
        let digest=info["record_sha256"].as_str().filter(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit())).ok_or("stroke_source_digest_invalid")?;
        visual["_stroke_record_sha256"]=json!(digest);
        Ok(Some(format!("“{character}”共有 {count} 笔。我会按本地笔顺资料展示，红点表示起笔位置。")))
    }else{
        visual["_stroke_unavailable_reason"]=info["unavailable_reason"].clone();
        Ok(Some(format!("我先展示“{character}”的字形。这个字的可靠笔顺资料暂时不可用，现在还不能演示笔顺。")))
    }
}

pub(crate) async fn prepare_with_tasks(
    models:&Arc<ModelWorker>,tasks:&Arc<super::tasks::TaskService>,root:&Path,visual:Option<&Value>,evidence:&[ToolEvidence],transcript:&str,hosts:&[String],
)->Result<Option<(PreparedImage,Option<String>)>,String>{
    let Some(visual)=visual else{return Ok(None)};
    match visual["kind"].as_str().unwrap_or("none"){
        "text"|"spelling"|"diagram"|"stroke"=>Ok(Some((super::capabilities::render_preview(tasks,visual).await?,None))),
        "artifact"|"media"=>{
            let id=visual["poster_artifact_id"].as_str().or(visual["artifact_id"].as_str()).ok_or("image_artifact_missing")?;
            let asset=tasks.artifacts.verify(id)?;let width=asset.source.pointer("/metadata/width").and_then(Value::as_u64).unwrap_or(0);
            let height=asset.source.pointer("/metadata/height").and_then(Value::as_u64).unwrap_or(0);
            if asset.mime_type!="image/jpeg"||asset.bytes>65_536||width==0||width>480||height==0||height>272{return Err("image_artifact_requires_prepare".into());}
            let bytes=tasks.artifacts.read(id,0,65_536)?;
            Ok(Some((PreparedImage{bytes,width:width as u32,height:height as u32,digest:asset.digest,title:asset.title},None)))
        },
        _=>prepare(models,root,Some(visual),evidence,transcript,hosts).await,
    }
}

pub(crate) async fn prepare(
    models: &Arc<ModelWorker>, root: &Path, visual: Option<&Value>, evidence: &[ToolEvidence], transcript: &str, hosts:&[String],
) -> Result<Option<(PreparedImage,Option<String>)>,String> {
    let Some(visual)=visual else {return Ok(None)};
    let (request,title,conclusion)=match visual["kind"].as_str().unwrap_or("none") {
        "none" => return Ok(None),
        "photo" => {
            let source=visual["source"].as_str().ok_or("image_source_missing")?;
            let bytes=if source.starts_with("https://") {
                verified_remote(source,hosts,evidence).await?
            } else {
                verified_file(root,source,evidence)?
            };
            (json!({"kind":"photo","image_base64":STANDARD.encode(bytes)}),"图片",None)
        },
        "bars" => {
            let (request,conclusion)=chart_request(visual,evidence,transcript)?;
            (request,"数据比较",Some(conclusion))
        },
        _ => return Err("image_kind_invalid".into()),
    };
    let result=tokio::time::timeout(Duration::from_secs(3),models.call("display_image",request))
        .await.map_err(|_|"image_prepare_timeout")??;
    let width=result["width"].as_u64().filter(|n|*n>0 && *n<=480).ok_or("image_width_invalid")? as u32;
    let height=result["height"].as_u64().filter(|n|*n>0 && *n<=272).ok_or("image_height_invalid")? as u32;
    let encoded=result["jpeg_base64"].as_str().ok_or("image_data_missing")?;
    if encoded.len()>4*((MAX_OUTPUT_BYTES+2)/3) {return Err("image_output_size".into());}
    let bytes=STANDARD.decode(encoded).map_err(|_|"image_output_invalid")?;
    if bytes.is_empty() || bytes.len()>MAX_OUTPUT_BYTES {return Err("image_output_size".into());}
    let digest=format!("{:x}",Sha256::digest(&bytes));
    Ok(Some((PreparedImage {bytes,width,height,digest,title:title.into()},conclusion)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_extra_illustration_does_not_fail_a_text_answer() {
        let older = json!({"kind":"bars"});
        assert!(!image_required(Some(&older), "answered", "none"));
        assert!(image_required(Some(&older), "done", "read"));
        assert!(image_required(Some(&json!({"kind":"none","requested":true})), "answered", "none"));
        assert!(!image_required(Some(&json!({"kind":"bars","requested":false})), "done", "read"));
    }

    #[test]
    fn chart_requires_the_current_tool_data() {
        let visual=json!({"unit":"厘米","series":[{"label":"红盆","value":7},{"label":"蓝盆","value":11}]});
        let evidence=ToolEvidence{name:"read".into(),input:json!({}),output:"盆名,高度_厘米\n红盆,7\n蓝盆,11".into()};
        let (_,summary)=chart_request(&visual,&[evidence.clone()],"比较一下").unwrap();
        assert!(summary.contains("蓝盆多 4厘米"));
        let changed=json!({"unit":"厘米","series":[{"label":"红盆","value":17},{"label":"蓝盆","value":11}]});
        assert!(chart_request(&changed,&[evidence],"比较一下").is_err());
    }

    #[test]
    fn a_read_result_does_not_prove_a_write() {
        let read=ToolEvidence{name:"read".into(),input:json!({}),output:String::new()};
        assert!(confirmed_effect("read",&[read.clone()]));
        assert!(!confirmed_effect("write",&[read]));
        let write=ToolEvidence{name:"edit".into(),input:json!({}),output:String::new()};
        assert!(confirmed_effect("write",&[write]));
        assert!(!confirmed_effect("other",&[]));
    }

    #[test]
    fn image_hosts_and_local_addresses_are_bounded() {
        let hosts=vec!["images.example.org".to_owned()];
        assert!(allowed_url(&reqwest::Url::parse("https://images.example.org/a.jpg").unwrap(),&hosts));
        assert!(!allowed_url(&reqwest::Url::parse("http://images.example.org/a.jpg").unwrap(),&hosts));
        assert!(!allowed_url(&reqwest::Url::parse("https://localhost/a.jpg").unwrap(),&["localhost".into()]));
        assert!(!public_address("127.0.0.1".parse().unwrap()));
        assert!(!public_address("192.168.1.2".parse().unwrap()));
        assert!(public_address("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn local_images_require_a_matching_completed_tool_result() {
        let root=std::env::temp_dir().join(format!("xiaodai-display-{}",uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&root).unwrap();
        let photo=root.join("sample.jpg");
        std::fs::write(&photo,b"sample").unwrap();
        let source=photo.to_string_lossy().to_string();
        let evidence=ToolEvidence{name:"glob".into(),input:json!({}),output:source.clone()};
        assert_eq!(verified_file(&root,&source,&[evidence]).unwrap(),b"sample");
        assert!(verified_file(&root,&source,&[]).is_err());
        assert!(verified_file(&root,"../outside.jpg",&[]).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
