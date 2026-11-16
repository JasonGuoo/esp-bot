//! Immutable, content-addressed local assets. Paths are never supplied by a model.
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, sync::Mutex};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: String,
    pub digest: String,
    pub mime_type: String,
    pub bytes: u64,
    pub title: String,
    pub source: Value,
    pub created_at_ms: i64,
}

pub struct ArtifactStore { root: PathBuf, db: Mutex<Connection>, max_bytes: u64 }

impl ArtifactStore {
    pub fn open(root: PathBuf, max_bytes: u64) -> Result<Self, String> {
        super::settings::private_dir(&root)?;
        let path=root.join("manifest.sqlite3");
        let db=Connection::open(&path).map_err(err)?;
        private_permissions(&path)?;
        db.busy_timeout(std::time::Duration::from_secs(2)).map_err(err)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY, digest TEXT NOT NULL, mime_type TEXT NOT NULL,
                bytes INTEGER NOT NULL, title TEXT NOT NULL, source_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL, removed INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX IF NOT EXISTS artifact_recent ON artifacts(created_at_ms);
            CREATE TABLE IF NOT EXISTS artifact_invalidations(id TEXT PRIMARY KEY, reason TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS source_blocks(source_id TEXT PRIMARY KEY);").map_err(err)?;
        let store=Self {root,db:Mutex::new(db),max_bytes};store.purge_orphans()?;Ok(store)
    }

    pub fn put(&self, bytes:&[u8], mime_type:&str, title:&str, source:Value) -> Result<Artifact,String> {
        if bytes.is_empty() || bytes.len() as u64>self.max_bytes {return Err("artifact_size_limit".into());}
        let extension=extension(mime_type).ok_or("artifact_mime_unsupported")?;
        if title.chars().count()>240 || serde_json::to_vec(&source).map_err(err)?.len()>65_536 {return Err("artifact_metadata_limit".into());}
        let digest=format!("{:x}",Sha256::digest(bytes));
        // A separate immutable record preserves the exact provenance even when
        // identical bytes are reused for two different household interactions.
        let id=format!("asset_{}",uuid::Uuid::new_v4().simple());
        let file=self.root.join(format!("{digest}.{extension}"));
        let db=self.db.lock().map_err(|_|"artifact_lock")?;
        for source_id in [source["conversation_turn_id"].as_str(),source["task_id"].as_str(),source["presentation_id"].as_str(),source.pointer("/context/conversation_turn_id").and_then(Value::as_str)].into_iter().flatten(){
            if db.query_row("SELECT EXISTS(SELECT 1 FROM source_blocks WHERE source_id=?1)",[source_id],|r|r.get::<_,bool>(0)).map_err(err)?{return Err("artifact_source_invalidated".into());}}
        let (count,stored):(i64,i64)=db.query_row("SELECT (SELECT count(*) FROM artifacts WHERE removed=0),COALESCE(sum(bytes),0) FROM (SELECT max(bytes) AS bytes FROM artifacts WHERE removed=0 GROUP BY digest,mime_type)",[],|r|Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
        let budget=self.max_bytes.saturating_mul(32).clamp(256*1024*1024,8*1024*1024*1024);
        if count>=50_000||(!file.exists()&&(stored as u64).saturating_add(bytes.len() as u64)>budget){return Err("artifact_capacity_reached".into());}
        if !file.exists() {
            let temporary=self.root.join(format!(".{}",uuid::Uuid::new_v4().simple()));
            let mut options=fs::OpenOptions::new();options.write(true).create_new(true);
            #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
            let mut out=options.open(&temporary).map_err(err)?;
            if let Err(e)=out.write_all(bytes).and_then(|_|out.sync_all()).and_then(|_|fs::rename(&temporary,&file)) {
                let _=fs::remove_file(&temporary);return Err(err(e));
            }
        }
        let now=crate::time::now_ms() as i64;
        db.execute("INSERT INTO artifacts(id,digest,mime_type,bytes,title,source_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id,digest,mime_type,bytes.len() as i64,title,serde_json::to_string(&source).map_err(err)?,now]).map_err(err)?;
        Ok(Artifact{artifact_id:id,digest,mime_type:mime_type.into(),bytes:bytes.len() as u64,title:title.into(),source,created_at_ms:now})
    }

    pub fn import(&self, path:&Path, mime_type:&str, title:&str, source:Value) -> Result<Artifact,String> {
        let file=fs::File::open(path).map_err(err)?;
        let mut bytes=Vec::new();file.take(self.max_bytes+1).read_to_end(&mut bytes).map_err(err)?;
        self.put(&bytes,mime_type,title,source)
    }

    pub fn get(&self,id:&str)->Result<Artifact,String> {
        self.get_internal(id,false)
    }
    fn get_internal(&self,id:&str,include_invalidated:bool)->Result<Artifact,String> {
        valid_id(id)?;
        let db=self.db.lock().map_err(|_|"artifact_lock")?;
        if !include_invalidated&&db.query_row("SELECT EXISTS(SELECT 1 FROM artifact_invalidations WHERE id=?1)",[id],|r|r.get::<_,bool>(0)).map_err(err)?{return Err("artifact_source_invalidated".into());}
        let record=db.query_row("SELECT id,digest,mime_type,bytes,title,source_json,created_at_ms FROM artifacts WHERE id=?1 AND removed=0",[id],|r|{
            let source:String=r.get(5)?;
            Ok(Artifact{artifact_id:r.get(0)?,digest:r.get(1)?,mime_type:r.get(2)?,bytes:r.get::<_,i64>(3)? as u64,
                title:r.get(4)?,source:serde_json::from_str(&source).unwrap_or(Value::Null),created_at_ms:r.get(6)?})
        }).optional().map_err(err)?;
        record.ok_or_else(||"artifact_missing_or_expired".into())
    }

    pub fn path(&self,id:&str)->Result<PathBuf,String> {
        let asset=self.get(id)?;
        let ext=extension(&asset.mime_type).ok_or("artifact_mime_unsupported")?;
        let path=self.root.join(format!("{}.{}",asset.digest,ext));
        if !path.is_file(){return Err("artifact_missing_or_expired".into());}
        Ok(path)
    }

    pub fn read(&self,id:&str,offset:u64,max_bytes:usize)->Result<Vec<u8>,String> {
        use std::io::{Seek,SeekFrom};
        if max_bytes==0 || max_bytes>1_048_576{return Err("artifact_read_limit".into());}
        let asset=self.get(id)?;
        if offset>asset.bytes{return Err("artifact_offset_invalid".into());}
        let mut input=fs::File::open(self.path(id)?).map_err(err)?;
        input.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut bytes=Vec::new();input.take(max_bytes as u64).read_to_end(&mut bytes).map_err(err)?;Ok(bytes)
    }

    pub fn list(&self,limit:usize)->Result<Vec<Artifact>,String> {
        let ids={let db=self.db.lock().map_err(|_|"artifact_lock")?;
            let mut statement=db.prepare("SELECT id FROM artifacts WHERE removed=0 AND id NOT IN (SELECT id FROM artifact_invalidations) ORDER BY created_at_ms DESC LIMIT ?1").map_err(err)?;
            let rows=statement.query_map([limit.clamp(1,200) as i64],|r|r.get::<_,String>(0)).map_err(err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?};
        ids.iter().map(|id|self.get(id)).collect()
    }

    pub fn forget(&self,id:&str)->Result<(),String> {
        let asset=self.get_internal(id,true)?;
        let db=self.db.lock().map_err(|_|"artifact_lock")?;
        let remaining:i64=db.query_row("SELECT count(*) FROM artifacts WHERE digest=?1 AND mime_type=?2 AND removed=0 AND id<>?3",params![asset.digest,asset.mime_type,id],|r|r.get(0)).map_err(err)?;
        if remaining==0 {
            if let Some(ext)=extension(&asset.mime_type){let path=self.root.join(format!("{}.{}",asset.digest,ext));
                if let Err(e)=fs::remove_file(path){if e.kind()!=std::io::ErrorKind::NotFound{return Err(err(e));}}
            }
        }
        // Keep provenance discoverable if unlink fails. Otherwise the cleanup
        // outbox's retry would skip removed=1 and falsely report erasure while
        // the final private blob still existed on disk.
        db.execute("UPDATE artifacts SET removed=1,title='',source_json='null' WHERE id=?1",[id]).map_err(err)?;
        Ok(())
    }

    pub fn invalidate_sources(&self,turn_ids:&[String],task_ids:&[String],all:bool)->Result<(),String>{
        self.block_sources(turn_ids,task_ids)?;
        let ids=self.source_ids(turn_ids,task_ids,all)?;let db=self.db.lock().map_err(|_|"artifact_lock")?;
        for id in ids{db.execute("INSERT OR IGNORE INTO artifact_invalidations(id,reason) VALUES(?1,'source_changed')",[id]).map_err(err)?;}Ok(())
    }
    pub fn forget_sources(&self,turn_ids:&[String],task_ids:&[String],all:bool)->Result<(),String>{
        self.block_sources(turn_ids,task_ids)?;
        for id in self.source_ids(turn_ids,task_ids,all)?{self.forget(&id)?;}if all{self.purge_orphans()?;}Ok(())
    }
    pub(crate) fn block_sources(&self,turn_ids:&[String],task_ids:&[String])->Result<(),String>{
        let db=self.db.lock().map_err(|_|"artifact_lock")?;for id in turn_ids.iter().chain(task_ids){valid_id(id)?;db.execute("INSERT OR IGNORE INTO source_blocks(source_id) VALUES(?1)",[id]).map_err(err)?;}Ok(())
    }
    pub(crate) fn source_ids(&self,turn_ids:&[String],task_ids:&[String],all:bool)->Result<Vec<String>,String>{
        let db=self.db.lock().map_err(|_|"artifact_lock")?;let mut statement=db.prepare("SELECT id,source_json FROM artifacts WHERE removed=0").map_err(err)?;
        let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(err)?;
        let mut ids=Vec::new();for row in rows{let(id,raw)=row.map_err(err)?;let source:Value=serde_json::from_str(&raw).unwrap_or(Value::Null);
            let turn=source["conversation_turn_id"].as_str().or_else(||source.pointer("/context/conversation_turn_id").and_then(Value::as_str));
            if all||turn.is_some_and(|id|turn_ids.iter().any(|t|t==id))||[source["task_id"].as_str(),source["presentation_id"].as_str()].into_iter().flatten().any(|id|task_ids.iter().any(|t|t==id)){ids.push(id);}}
        Ok(ids)
    }

    pub fn verify(&self,id:&str)->Result<Artifact,String>{
        let asset=self.get(id)?;let file=fs::File::open(self.path(id)?).map_err(err)?;
        let mut bytes=Vec::new();file.take(self.max_bytes+1).read_to_end(&mut bytes).map_err(err)?;
        if bytes.len() as u64!=asset.bytes || format!("{:x}",Sha256::digest(&bytes))!=asset.digest{return Err("artifact_digest_mismatch".into());}Ok(asset)
    }
    fn purge_orphans(&self)->Result<(),String>{
        let db=self.db.lock().map_err(|_|"artifact_lock")?;
        for entry in fs::read_dir(&self.root).map_err(err)?{let entry=entry.map_err(err)?;let name=entry.file_name();let Some(name)=name.to_str() else{continue};
            let temporary=name.len()==33&&name.starts_with('.')&&name[1..].bytes().all(|b|b.is_ascii_hexdigit());
            let blob=name.split_once('.').filter(|(digest,ext)|digest.len()==64&&digest.bytes().all(|b|b.is_ascii_hexdigit())&&!ext.is_empty());
            let orphan=if let Some((digest,_))=blob{!db.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE digest=?1 AND removed=0)",[digest],|r|r.get::<_,bool>(0)).map_err(err)?}else{false};
            if temporary||orphan{let kind=entry.file_type().map_err(err)?;if kind.is_file()||kind.is_symlink(){fs::remove_file(entry.path()).map_err(err)?;}}
        }Ok(())
    }
}

pub(crate) fn extension(mime:&str)->Option<&'static str>{Some(match mime{
    "image/jpeg"=>"jpg","image/png"=>"png","image/svg+xml"=>"svg","application/pdf"=>"pdf",
    "audio/wav"=>"wav","audio/mpeg"=>"mp3","video/mp4"=>"mp4","video/x-motion-jpeg"=>"mjpeg",
    "application/json"=>"json","text/plain"=>"txt","text/vtt"=>"vtt",_=>return None})}
pub(crate) fn valid_id(id:&str)->Result<(),String>{
    if id.is_empty()||id.len()>160||!id.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-'|b':'|b'.')){Err("identifier_invalid".into())}else{Ok(())}}
fn private_permissions(path:&Path)->Result<(),String>{
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;fs::set_permissions(path,fs::Permissions::from_mode(0o600)).map_err(err)?;}Ok(())}
fn err(e:impl std::fmt::Display)->String{format!("artifact_store: {e}")}
