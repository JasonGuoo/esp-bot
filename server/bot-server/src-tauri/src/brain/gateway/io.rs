//! A single bounded writer owns the WSS sink. Admission never waits for network
//! progress; a queued frame is not a transport or hardware receipt.
use std::{sync::{Arc, atomic::{AtomicU64, Ordering}}, time::Duration};
use futures_util::{Sink, SinkExt};
use tokio::{sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore}, task::JoinHandle, time::timeout};
use tokio_tungstenite::tungstenite::Message;

pub(super) const MAX_QUEUED_FRAMES: usize = 32;
pub(super) const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
const WRITE_DEADLINE: Duration = Duration::from_secs(3);

struct Frame {
    message: Message,
    generation: Option<u64>,
    _bytes: OwnedSemaphorePermit,
}

pub(super) struct Writer<S> {
    direct: Option<S>,
    queued: Option<Queued>,
}

struct Queued {
    tx: mpsc::Sender<Frame>,
    bytes: Arc<Semaphore>,
    generation: Arc<AtomicU64>,
    failed: watch::Receiver<Option<&'static str>>,
    worker: JoinHandle<()>,
}

impl Drop for Queued {
    fn drop(&mut self) {
        // Covers error returns and service cancellation as well as normal close.
        self.worker.abort();
    }
}

impl<S> Writer<S>
where S: Sink<Message> + Send + Unpin + 'static, S::Error: Send {
    pub(super) fn new(sink: S, isolated: bool) -> Self {
        if !isolated { return Self { direct: Some(sink), queued: None }; }
        let (tx, rx)=mpsc::channel(MAX_QUEUED_FRAMES);
        let (status,failed)=watch::channel(None);
        let generation=Arc::new(AtomicU64::new(1));
        let worker_generation=Arc::clone(&generation);
        let worker=tokio::spawn(async move {
            let error=write_frames(sink,rx,worker_generation,WRITE_DEADLINE).await;
            status.send_replace(Some(error));
        });
        Self { direct: None, queued: Some(Queued {
            tx, bytes: Arc::new(Semaphore::new(MAX_QUEUED_BYTES)), generation, failed, worker,
        }) }
    }

    /// Invalidate frames the writer has not begun. An in-progress write is bounded
    /// by WRITE_DEADLINE; interrupting and then reusing a partial WSS write would
    /// corrupt framing. The device still owns immediate local stop.
    pub(super) fn fence(&self) {
        if let Some(queue)=&self.queued { queue.generation.fetch_add(1,Ordering::AcqRel); }
    }

    pub(super) async fn send(&mut self,message:Message)->Result<(),String> {
        if let Some(sink)=self.direct.as_mut() {
            return timeout(WRITE_DEADLINE,sink.send(message)).await
                .map_err(|_|"ws_write_timeout".to_owned())?
                .map_err(|_|"ws_write_failed".to_owned());
        }
        let queue=self.queued.as_ref().ok_or("ws_writer_closed")?;
        if let Some(error)=*queue.failed.borrow() { return Err(error.into()); }
        let size=message.len().max(1);
        if size>super::MAX_JSON { return Err("ws_frame_limit".into()); }
        let bytes=Arc::clone(&queue.bytes).try_acquire_many_owned(size as u32)
            .map_err(|_|"ws_egress_bytes_full")?;
        let generation=output_scoped(&message).then(||queue.generation.load(Ordering::Acquire));
        let frame=Frame {message,generation,_bytes:bytes};
        queue.tx.try_send(frame).map_err(|error|match error {
            mpsc::error::TrySendError::Full(_)=>"ws_egress_full".into(),
            mpsc::error::TrySendError::Closed(_)=>"ws_writer_closed".into(),
        })
    }

    /// Separate from the full data queue, so write failure always wakes control.
    pub(super) async fn failed(&mut self) {
        let Some(queue)=self.queued.as_mut() else { return std::future::pending().await };
        while queue.failed.borrow().is_none() {
            if queue.failed.changed().await.is_err() { break; }
        }
    }
}

async fn write_frames<S>(mut sink:S,mut rx:mpsc::Receiver<Frame>,generation:Arc<AtomicU64>,deadline:Duration)->&'static str
where S:Sink<Message>+Unpin {
    while let Some(frame)=rx.recv().await {
        if frame.generation.is_some_and(|value|value!=generation.load(Ordering::Acquire)) { continue; }
        match timeout(deadline,sink.send(frame.message)).await {
            Ok(Ok(()))=>{},
            Ok(Err(_))=>return "ws_write_failed",
            Err(_)=>return "ws_write_timeout",
        }
    }
    "ws_writer_closed"
}

/// The legacy wire adapter reads only the trusted top-level envelope, never
/// words inside speech or payload. Ordinary RPC replies and terminal failures
/// must survive an output fence; queued speech/media must not.
fn output_scoped(message:&Message)->bool {
    #[derive(serde::Deserialize)]
    struct Envelope<'a> {
        event: Option<&'a str>,
        runtime: Option<serde::de::IgnoredAny>,
        ok: Option<bool>,
    }
    match message {
        Message::Binary(_)=>true,
        Message::Text(text)=>serde_json::from_str::<Envelope<'_>>(text).is_ok_and(|envelope| {
            envelope.runtime.is_some() && envelope.ok!=Some(false)
                && !matches!(envelope.event,Some("session.cancelled"|"session.expired"|"audio.cancelled"))
        }),
        _=>false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio_tungstenite::{WebSocketStream,tungstenite::protocol::Role};

    #[tokio::test]
    async fn blocked_socket_does_not_block_fence_or_queue_admission() {
        let (server,client)=tokio::io::duplex(64);
        let server=WebSocketStream::from_raw_socket(server,Role::Server,None).await;
        let mut client=WebSocketStream::from_raw_socket(client,Role::Client,None).await;
        let (sink,_)=server.split();
        let mut writer=Writer::new(sink,true);
        writer.send(Message::Binary(vec![1;4096])).await.unwrap();
        // A real bounded transport stops the writer before the client reads.
        tokio::task::yield_now().await;
        writer.send(Message::Text(r#"{"event":"presentation.segment","runtime":{"epoch":1}}"#.into())).await.unwrap();
        writer.send(Message::Text(r#"{"type":"response","id":"status","ok":true}"#.into())).await.unwrap();
        writer.fence();
        timeout(Duration::from_millis(100),writer.send(Message::Text("stopped".into()))).await.unwrap().unwrap();
        assert!(matches!(client.next().await,Some(Ok(Message::Binary(_)))));
        assert_eq!(client.next().await.unwrap().unwrap(),Message::Text(r#"{"type":"response","id":"status","ok":true}"#.into()));
        assert_eq!(client.next().await.unwrap().unwrap(),Message::Text("stopped".into()));
    }

    #[tokio::test]
    async fn stalled_writer_has_both_frame_and_byte_limits() {
        let (server,_client)=tokio::io::duplex(64);
        let server=WebSocketStream::from_raw_socket(server,Role::Server,None).await;
        let (sink,_)=server.split();
        let mut writer=Writer::new(sink,true);
        writer.send(Message::Binary(vec![0;4096])).await.unwrap();
        tokio::task::yield_now().await;
        for _ in 0..MAX_QUEUED_FRAMES { writer.send(Message::Text("x".into())).await.unwrap(); }
        assert_eq!(writer.send(Message::Text("overflow".into())).await.unwrap_err(),"ws_egress_full");
        drop(writer);

        let (server,_client)=tokio::io::duplex(64);
        let server=WebSocketStream::from_raw_socket(server,Role::Server,None).await;
        let (sink,_)=server.split();
        let mut writer=Writer::new(sink,true);
        writer.send(Message::Binary(vec![0;super::super::MAX_JSON])).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(writer.send(Message::Binary(vec![0;super::super::MAX_JSON])).await.unwrap_err(),"ws_egress_bytes_full");
    }

    #[tokio::test]
    async fn network_deadline_reports_failure_outside_the_data_queue() {
        let (server,_client)=tokio::io::duplex(64);
        let sink=WebSocketStream::from_raw_socket(server,Role::Server,None).await;
        let (tx,rx)=mpsc::channel(1);
        let bytes=Arc::new(Semaphore::new(4096));
        tx.try_send(Frame { message:Message::Binary(vec![0;4096]),generation:Some(1),
            _bytes:bytes.acquire_many_owned(4096).await.unwrap() }).unwrap_or_else(|_|panic!("empty queue"));
        assert_eq!(write_frames(sink,rx,Arc::new(AtomicU64::new(1)),Duration::from_millis(20)).await,"ws_write_timeout");
    }
}
