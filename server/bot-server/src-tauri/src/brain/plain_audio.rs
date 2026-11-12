//! Per-turn LAN audio socket. PCM is deliberately plaintext; a short-lived
//! HMAC key delivered over the authenticated WSS control connection protects
//! frame integrity without disclosing the reusable device credential.
use std::{net::{IpAddr, SocketAddr}, time::Duration};
use ring::{hmac, rand::{SecureRandom, SystemRandom}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener,
    sync::mpsc, task::JoinHandle, time::timeout};
use zeroize::Zeroizing;

const MAX_FRAME: usize = 2048;
const MAC_LEN: usize = 32;

pub enum Input { Frame(Vec<u8>), Closed }
pub enum Output { Frame(Vec<u8>), Finish { stream:u32, frames:u32 } }

pub struct PlainAudio {
    pub host: String,
    pub port: u16,
    pub key_hex: Zeroizing<String>,
    pub incoming: mpsc::Receiver<Input>,
    pub outgoing: mpsc::Sender<Output>,
    task: JoinHandle<()>,
}

impl Drop for PlainAudio { fn drop(&mut self) { self.task.abort(); } }

fn frame(key:&hmac::Key, kind:u8, payload:&[u8])->Vec<u8> {
    let mut body=Vec::with_capacity(1+payload.len()+MAC_LEN);
    body.push(kind);body.extend_from_slice(payload);
    let tag=hmac::sign(key,&body);
    body.extend_from_slice(tag.as_ref());
    let mut wire=Vec::with_capacity(4+body.len());
    wire.extend_from_slice(&(body.len() as u32).to_be_bytes());
    wire.extend_from_slice(&body);
    wire
}

pub async fn start(local:IpAddr, peer:IpAddr)->Result<PlainAudio,String> {
    if !local.is_ipv4() || !peer.is_ipv4() {return Err("plain_audio_requires_ipv4".into());}
    let listener=TcpListener::bind(SocketAddr::new(local,0)).await.map_err(|_|"plain_audio_bind_failed")?;
    let address=listener.local_addr().map_err(|_|"plain_audio_bind_failed")?;
    let mut secret=Zeroizing::new([0u8;32]);
    SystemRandom::new().fill(secret.as_mut()).map_err(|_|"plain_audio_random_failed")?;
    let key=hmac::Key::new(hmac::HMAC_SHA256,secret.as_ref());
    let key_hex=Zeroizing::new(secret.iter().map(|b|format!("{b:02x}")).collect::<String>());
    let (input_tx,incoming)=mpsc::channel::<Input>(128);
    let (outgoing,mut output_rx)=mpsc::channel::<Output>(32);
    let task=tokio::spawn(async move {
        let accepted=timeout(Duration::from_secs(10),listener.accept()).await;
        let Ok(Ok((stream,address)))=accepted else {let _=input_tx.send(Input::Closed).await;return};
        if address.ip()!=peer {let _=input_tx.send(Input::Closed).await;return;}
        let (mut read,mut write)=stream.into_split();
        let read_key=key.clone();
        let reader=async move {
            loop {
                let mut len=[0u8;4];
                if read.read_exact(&mut len).await.is_err(){break;}
                let len=u32::from_be_bytes(len) as usize;
                if !(1+MAC_LEN..=MAX_FRAME).contains(&len){break;}
                let mut body=vec![0u8;len];
                if read.read_exact(&mut body).await.is_err(){break;}
                let split=len-MAC_LEN;
                if body[0]!=b'I' || hmac::verify(&read_key,&body[..split],&body[split..]).is_err(){break;}
                if input_tx.send(Input::Frame(body[1..split].to_vec())).await.is_err(){return;}
            }
            let _=input_tx.send(Input::Closed).await;
        };
        let writer=async move {
            while let Some(output)=output_rx.recv().await {
                let (kind,payload,done)=match output {
                    Output::Frame(data)=>(b'O',data,false),
                    Output::Finish{stream,frames}=>{
                        let mut data=Vec::with_capacity(8);
                        data.extend_from_slice(&stream.to_le_bytes());
                        data.extend_from_slice(&frames.to_le_bytes());
                        (b'E',data,true)
                    }
                };
                let bytes=frame(&key,kind,&payload);
                if timeout(Duration::from_secs(2),write.write_all(&bytes)).await.ok().and_then(Result::ok).is_none(){break;}
                if done {let _=write.shutdown().await;break;}
            }
        };
        // Both halves live inside this one task. Dropping the WSS session
        // aborts this task and closes both halves immediately.
        tokio::select! {_=reader=>{},_=writer=>{}}
    });
    Ok(PlainAudio {host:address.ip().to_string(),port:address.port(),key_hex,incoming,outgoing,task})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_audio_authenticates_frame_and_direction() {
        let key=hmac::Key::new(hmac::HMAC_SHA256,b"temporary test key");
        let mut encoded=frame(&key,b'I',b"pcm");
        assert!(hmac::verify(&key,&encoded[4..8],&encoded[8..]).is_ok());
        encoded[4]=b'O';
        assert!(hmac::verify(&key,&encoded[4..8],&encoded[8..]).is_err());
    }
    #[tokio::test]
    async fn local_tcp_transports_plain_pcm_and_rejects_tampering() {
        let local:IpAddr="127.0.0.1".parse().unwrap();
        let mut transport=start(local,local).await.unwrap();
        let secret=(0..32).map(|i|u8::from_str_radix(&transport.key_hex[2*i..2*i+2],16).unwrap()).collect::<Vec<_>>();
        let key=hmac::Key::new(hmac::HMAC_SHA256,&secret);
        let mut client=tokio::net::TcpStream::connect((local,transport.port)).await.unwrap();
        let wire=frame(&key,b'I',b"synthetic-pcm");
        client.write_all(&wire[..3]).await.unwrap();
        client.write_all(&wire[3..]).await.unwrap();
        let incoming=timeout(Duration::from_secs(2),transport.incoming.recv()).await.unwrap();
        assert!(matches!(incoming,Some(Input::Frame(ref data)) if data==b"synthetic-pcm"));
        transport.outgoing.send(Output::Frame(b"synthetic-reply".to_vec())).await.unwrap();
        let mut len=[0u8;4];client.read_exact(&mut len).await.unwrap();
        let mut reply=vec![0;u32::from_be_bytes(len) as usize];client.read_exact(&mut reply).await.unwrap();
        let split=reply.len()-MAC_LEN;
        assert_eq!(&reply[..split],b"Osynthetic-reply");
        assert!(hmac::verify(&key,&reply[..split],&reply[split..]).is_ok());
        let mut invalid=frame(&key,b'I',b"synthetic-pcm");invalid[6]^=1;
        client.write_all(&invalid).await.unwrap();
        assert!(matches!(timeout(Duration::from_secs(2),transport.incoming.recv()).await.unwrap(),Some(Input::Closed)));
    }
}
