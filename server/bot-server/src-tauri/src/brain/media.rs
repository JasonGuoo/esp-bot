//! Bounded PCM16 media framing for the authenticated v3 gateway.
use base64::{Engine, engine::general_purpose::STANDARD};

pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_INPUT_PCM: usize = 30 * SAMPLE_RATE as usize * 2;
pub const MAX_OUTPUT_PCM: usize = 120 * SAMPLE_RATE as usize * 2;
pub const HEADER: usize = 20;

pub(super) struct CandidateAsr(tokio::task::JoinHandle<Result<serde_json::Value,String>>);

impl CandidateAsr {
    pub async fn result(mut self) -> Option<serde_json::Value> {
        (&mut self.0).await.ok()?.ok()
    }
}
impl Drop for CandidateAsr {
    fn drop(&mut self) { self.0.abort(); }
}

pub struct AudioInput {
    pub stream_id: u32,
    pub sequence: u32,
    pub last_capture_ms: u64,
    pub pcm: Vec<u8>,
    pub started: std::time::Instant,
    pause_at: Option<u32>,
    attempted: bool,
    pub(super) candidate: Option<CandidateAsr>,
}

impl AudioInput {
    pub fn new(stream_id: u32) -> Self {
        Self { stream_id, sequence:0, last_capture_ms:0, pcm:Vec::new(), started:std::time::Instant::now(),pause_at:None,attempted:false,candidate:None }
    }

    /// The pause arrives on WSS, potentially before its last PCM frame on TCP.
    /// A capture gets one speculative request; a resumed utterance invalidates it.
    pub(super) fn note_pause(&mut self, frames:u32) {
        if !self.attempted && self.pause_at.is_none() && frames>=self.sequence && frames>0 {
            self.pause_at=Some(frames);
        }
    }

    pub(super) fn start_candidate(&mut self, worker:&std::sync::Arc<super::ModelWorker>) {
        if self.attempted || self.pause_at!=Some(self.sequence) || self.pcm.len()<1600 {return;}
        self.attempted=true;
        let worker=std::sync::Arc::clone(worker);
        let wav=STANDARD.encode(pcm_wav(&self.pcm));
        self.candidate=Some(CandidateAsr(tokio::spawn(async move {worker.transcribe_if_idle(wav).await})));
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() < HEADER + 2 || bytes.len() > HEADER + 1280 || &bytes[..4] != b"XDAI" { return Err("audio_frame_invalid".into()); }
        let stream = u32::from_le_bytes(bytes[4..8].try_into().map_err(|_|"audio_stream_header")?);
        let sequence = u32::from_le_bytes(bytes[8..12].try_into().map_err(|_|"audio_sequence_header")?);
        let capture_ms = u64::from_le_bytes(bytes[12..20].try_into().map_err(|_|"audio_time_header")?);
        let pcm = &bytes[HEADER..];
        if stream != self.stream_id || sequence != self.sequence { return Err("audio_sequence_mismatch".into()); }
        if pcm.len() % 2 != 0 || self.pcm.len() + pcm.len() > MAX_INPUT_PCM
            || (self.sequence > 0 && self.started.elapsed().as_secs() > 35) { return Err("audio_limit_exceeded".into()); }
        if sequence > 0 && capture_ms < self.last_capture_ms { return Err("audio_timestamp_regressed".into()); }
        // An explicitly activated follow-up may prepare its transport while
        // the user reads. The recording budget begins with actual PCM.
        if self.sequence == 0 { self.started = std::time::Instant::now(); }
        self.pcm.extend_from_slice(pcm); self.sequence += 1; self.last_capture_ms = capture_ms;
        if self.pause_at.is_some_and(|at|self.sequence>at) {
            self.candidate.take();
            self.pause_at=None;
        }
        Ok(())
    }

    pub fn wav_base64(self) -> Result<String, String> {
        if self.pcm.len() < 1600 { return Err("audio_too_short".into()); }
        Ok(STANDARD.encode(pcm_wav(&self.pcm)))
    }
}

pub fn pcm_wav(pcm: &[u8]) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend(b"RIFF"); wav.extend((36 + pcm.len() as u32).to_le_bytes()); wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes()); wav.extend(1u16.to_le_bytes()); wav.extend(1u16.to_le_bytes());
    wav.extend(SAMPLE_RATE.to_le_bytes()); wav.extend((SAMPLE_RATE*2).to_le_bytes());
    wav.extend(2u16.to_le_bytes()); wav.extend(16u16.to_le_bytes()); wav.extend(b"data");
    wav.extend((pcm.len() as u32).to_le_bytes()); wav.extend(pcm); wav
}

pub fn decode_output(wav_base64: &str) -> Result<Vec<u8>, String> {
    if wav_base64.len() > 6 * 1024 * 1024 { return Err("tts_audio_too_large".into()); }
    let wav = STANDARD.decode(wav_base64).map_err(|_|"tts_wav_base64_invalid")?;
    if wav.len() < 44 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" { return Err("tts_wav_header_invalid".into()); }
    let mut offset = 12; let mut valid_format = false; let mut data = None;
    while offset + 8 <= wav.len() {
        let size = u32::from_le_bytes(wav[offset+4..offset+8].try_into().map_err(|_|"tts_wav_size")?) as usize;
        let end = offset.checked_add(8).and_then(|v|v.checked_add(size)).ok_or("tts_wav_size")?;
        if end > wav.len() { return Err("tts_wav_truncated".into()); }
        let chunk = &wav[offset+8..end];
        if &wav[offset..offset+4] == b"fmt " && size >= 16 {
            valid_format = chunk[0..2] == [1,0] && chunk[2..4] == [1,0]
                && chunk[4..8] == SAMPLE_RATE.to_le_bytes() && chunk[14..16] == [16,0];
        }
        if &wav[offset..offset+4] == b"data" { data = Some(chunk); }
        offset = end + (size & 1);
    }
    let data = data.ok_or("tts_wav_data_missing")?;
    if !valid_format || data.is_empty() || data.len() % 2 != 0 || data.len() > MAX_OUTPUT_PCM { return Err("tts_wav_format_or_limit".into()); }
    Ok(data.to_vec())
}

pub fn output_frame(stream: u32, sequence: u32, pcm: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + pcm.len()); frame.extend(b"XDAI");
    frame.extend(stream.to_le_bytes()); frame.extend(sequence.to_le_bytes());
    frame.extend((sequence as u64 * 20).to_le_bytes()); frame.extend(pcm); frame
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resumed_pcm_retires_the_exact_pause_candidate() {
        let mut capture=AudioInput::new(7);
        capture.note_pause(2); // control and PCM may arrive on different sockets
        capture.push(&output_frame(7,0,&[0;960])).unwrap();
        assert_ne!(capture.pause_at,Some(capture.sequence));
        capture.push(&output_frame(7,1,&[0;960])).unwrap();
        assert_eq!(capture.pause_at,Some(capture.sequence));
        capture.attempted=true;
        let job=tokio::spawn(std::future::pending());
        let handle=job.abort_handle();
        capture.candidate=Some(CandidateAsr(job));
        capture.push(&output_frame(7,2,&[1;960])).unwrap();
        tokio::task::yield_now().await;
        assert!(handle.is_finished());
        assert!(capture.candidate.is_none());
        capture.note_pause(3);
        assert_eq!(capture.pause_at,None);
        assert_eq!(capture.pcm.len(),2880);
    }
}
