use crate::{read_message,write_message,Bounds,Message};
use std::{io,time::Duration};
use subtle::ConstantTimeEq;
use tokio::{io::{AsyncRead,AsyncWrite,WriteHalf},sync::mpsc,task::JoinHandle,time::timeout};

pub const HANDSHAKE_TIMEOUT:Duration=Duration::from_secs(3);
pub const WRITE_TIMEOUT:Duration=Duration::from_millis(500);
pub const HEARTBEAT_INTERVAL:Duration=Duration::from_millis(200);

fn bad()->io::Error{io::Error::new(io::ErrorKind::PermissionDenied,"handshake rejected")}
pub async fn host_handshake<S:AsyncRead+AsyncWrite+Unpin>(s:&mut S,secret:[u8;32],bounds:Bounds)->io::Result<Bounds> {
    match read_message(s).await? {Message::Hello(actual) if bool::from(actual.ct_eq(&secret))=>{},_=>return Err(bad())}
    write_message(s,Message::Welcome).await?;write_message(s,Message::Screen(bounds)).await?;
    let client=match read_message(s).await?{Message::Screen(b)=>b,_=>return Err(bad())};
    if read_message(s).await?!=Message::Ready{return Err(bad());}
    write_message(s,Message::Ready).await?;Ok(client)
}
pub async fn client_handshake<S:AsyncRead+AsyncWrite+Unpin>(s:&mut S,secret:[u8;32],bounds:Bounds)->io::Result<Bounds> {
    write_message(s,Message::Hello(secret)).await?;
    if read_message(s).await?!=Message::Welcome{return Err(bad());}
    let host=match read_message(s).await?{Message::Screen(b)=>b,_=>return Err(bad())};
    write_message(s,Message::Screen(bounds)).await?;write_message(s,Message::Ready).await?;
    if read_message(s).await?!=Message::Ready{return Err(bad());}Ok(host)
}

/// Owns a persistent reader so select! never cancels a partial TCP frame.
pub struct Connection<S> {pub incoming:mpsc::Receiver<io::Result<Message>>,writer:WriteHalf<S>,reader:JoinHandle<()>}
impl<S:AsyncRead+AsyncWrite+Unpin+Send+'static> Connection<S> {
    pub fn new(stream:S)->Self {
        let (mut read,writer)=tokio::io::split(stream);let(tx,incoming)=mpsc::channel(256);
        let reader=tokio::spawn(async move {loop {let m=read_message(&mut read).await;let done=m.is_err();if tx.send(m).await.is_err()||done{break;}}});
        Self{incoming,writer,reader}
    }
    pub async fn send(&mut self,m:Message)->io::Result<()> {
        timeout(WRITE_TIMEOUT,write_message(&mut self.writer,m)).await.map_err(|_|io::Error::new(io::ErrorKind::TimedOut,"write timeout"))?
    }
}
impl<S> Drop for Connection<S> {fn drop(&mut self){self.reader.abort();}}
