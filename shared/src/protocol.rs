//! Fixed-size, little-endian wire format. No native struct layout is serialized.
use std::io::{self, ErrorKind};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u8 = 1;
pub const MAX_PAYLOAD: usize = 32;
pub const HEADER_LEN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
impl Bounds {
    pub fn valid(self) -> bool {
        (2..=32768).contains(&self.width)
            && (2..=32768).contains(&self.height)
            && self.left.checked_add(self.width as i32).is_some()
            && self.top.checked_add(self.height as i32).is_some()
    }
    pub fn normalize_y(self, y: i32) -> u16 {
        (((i64::from(y) - i64::from(self.top)).clamp(0, i64::from(self.height - 1))
            * 65535) / i64::from(self.height - 1)) as u16
    }
    pub fn denormalize_y(self, y: u16) -> i32 {
        self.top + (u64::from(y) * u64::from(self.height - 1) / 65535) as i32
    }
    pub fn right(self) -> i32 { self.left + self.width as i32 - 1 }
}

// Deliberately no Debug: authentication and input must never enter logs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Hello([u8; 32]),
    Welcome,
    Screen(Bounds),
    Ready,
    Enter { epoch: u64, y: u16 },
    Return { epoch: u64, y: u16 },
    Move { dx: i32, dy: i32 },
    Key { vk: u16, scan: u16, extended: bool, down: bool },
    Button { button: u8, down: bool },
    Scroll { horizontal: bool, delta: i16 },
    Heartbeat(u64),
    ReleaseAll,
}

fn invalid() -> io::Error { io::Error::new(ErrorKind::InvalidData, "invalid protocol frame") }

impl Message {
    pub fn encode(self, out: &mut [u8; HEADER_LEN + MAX_PAYLOAD]) -> usize {
        let (kind, n) = match self {
            Self::Hello(_) => (1,32), Self::Welcome => (2,0), Self::Screen(_) => (3,16),
            Self::Ready => (4,0), Self::Enter{..} => (5,10), Self::Return{..} => (6,10),
            Self::Move{..} => (7,8), Self::Key{..} => (8,6), Self::Button{..} => (9,2),
            Self::Scroll{..} => (10,3), Self::Heartbeat(_) => (11,8), Self::ReleaseAll => (12,0),
        };
        out[..8].copy_from_slice(&[b'S', b'K', VERSION, kind, n as u8, 0, 0, 0]);
        let p = &mut out[8..];
        match self {
            Self::Hello(secret) => p[..32].copy_from_slice(&secret),
            Self::Screen(b) => {
                p[..4].copy_from_slice(&b.left.to_le_bytes()); p[4..8].copy_from_slice(&b.top.to_le_bytes());
                p[8..12].copy_from_slice(&b.width.to_le_bytes()); p[12..16].copy_from_slice(&b.height.to_le_bytes());
            }
            Self::Enter{epoch,y} | Self::Return{epoch,y} => {
                p[..8].copy_from_slice(&epoch.to_le_bytes()); p[8..10].copy_from_slice(&y.to_le_bytes());
            }
            Self::Move{dx,dy} => { p[..4].copy_from_slice(&dx.to_le_bytes()); p[4..8].copy_from_slice(&dy.to_le_bytes()); }
            Self::Key{vk,scan,extended,down} => { p[..2].copy_from_slice(&vk.to_le_bytes()); p[2..4].copy_from_slice(&scan.to_le_bytes()); p[4]=extended as u8; p[5]=down as u8; }
            Self::Button{button,down} => { p[0]=button; p[1]=down as u8; }
            Self::Scroll{horizontal,delta} => { p[0]=horizontal as u8; p[1..3].copy_from_slice(&delta.to_le_bytes()); }
            Self::Heartbeat(n) => p[..8].copy_from_slice(&n.to_le_bytes()),
            _ => {}
        }
        8+n
    }
    pub fn decode(header: [u8;8], p: &[u8]) -> io::Result<Self> {
        if header[..3] != [b'S', b'K', VERSION] || header[5..] != [0,0,0] || usize::from(header[4]) != p.len() { return Err(invalid()); }
        let u16_at = |i| u16::from_le_bytes([p[i],p[i+1]]);
        let u32_at = |i: usize| u32::from_le_bytes(p[i..i+4].try_into().unwrap());
        let u64_at = || u64::from_le_bytes(p[..8].try_into().unwrap());
        Ok(match (header[3],p.len()) {
            (1,32) => Self::Hello(p.try_into().unwrap()), (2,0) => Self::Welcome,
            (3,16) => { let b=Bounds{left:u32_at(0) as i32,top:u32_at(4) as i32,width:u32_at(8),height:u32_at(12)}; if !b.valid(){return Err(invalid());} Self::Screen(b) }
            (4,0) => Self::Ready,
            (5,10) => Self::Enter{epoch:u64_at(),y:u16_at(8)}, (6,10) => Self::Return{epoch:u64_at(),y:u16_at(8)},
            (7,8) => Self::Move{dx:u32_at(0) as i32,dy:u32_at(4) as i32},
            (8,6) if p[4]<=1 && p[5]<=1 && u16_at(0)<256 => Self::Key{vk:u16_at(0),scan:u16_at(2),extended:p[4]!=0,down:p[5]!=0},
            (9,2) if p[0]<5 && p[1]<=1 => Self::Button{button:p[0],down:p[1]!=0},
            (10,3) if p[0]<=1 => Self::Scroll{horizontal:p[0]!=0,delta:u16_at(1) as i16},
            (11,8) => Self::Heartbeat(u64_at()), (12,0) => Self::ReleaseAll,
            _ => return Err(invalid()),
        })
    }
}

/// Run this in a dedicated reader task: cancelling read_exact mid-frame loses framing.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Message> {
    let mut header = [0;8]; r.read_exact(&mut header).await?;
    let n=usize::from(header[4]); if n>MAX_PAYLOAD {return Err(invalid());}
    let mut payload=[0;MAX_PAYLOAD]; r.read_exact(&mut payload[..n]).await?;
    Message::decode(header,&payload[..n])
}
pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, m: Message) -> io::Result<()> {
    let mut buf=[0;HEADER_LEN+MAX_PAYLOAD]; let n=m.encode(&mut buf); w.write_all(&buf[..n]).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn round_trips() {
        let messages=[Message::Hello([9;32]),Message::Welcome,Message::Screen(Bounds{left:-1920,top:0,width:1920,height:1080}),Message::Ready,
            Message::Enter{epoch:9,y:65535},Message::Return{epoch:9,y:0},Message::Move{dx:-10,dy:20},
            Message::Key{vk:0xa2,scan:0x1d,extended:true,down:false},Message::Button{button:4,down:true},Message::Scroll{horizontal:true,delta:-120},Message::Heartbeat(42),Message::ReleaseAll];
        for m in messages { let mut b=[0;40]; let n=m.encode(&mut b); assert!(Message::decode(b[..8].try_into().unwrap(),&b[8..n]).unwrap()==m); }
    }
    #[test] fn rejects_bad_frames() {
        let mut b=[0;40]; let n=Message::Button{button:0,down:true}.encode(&mut b);
        b[2]=2; assert!(Message::decode(b[..8].try_into().unwrap(),&b[8..n]).is_err()); b[2]=1;
        b[9]=2; assert!(Message::decode(b[..8].try_into().unwrap(),&b[8..n]).is_err());
        assert!(Message::decode([b'S',b'K',1,8,1,0,0,0],&[0]).is_err());
    }
    #[tokio::test] async fn fragmented_and_concatenated_frames() {
        let (mut tx,mut rx)=tokio::io::duplex(64);
        let send=tokio::spawn(async move { let mut b=[0;40]; let n=Message::Heartbeat(7).encode(&mut b); for v in &b[..n] {tx.write_all(&[*v]).await.unwrap();} write_message(&mut tx,Message::ReleaseAll).await.unwrap(); });
        assert!(read_message(&mut rx).await.unwrap()==Message::Heartbeat(7)); assert!(read_message(&mut rx).await.unwrap()==Message::ReleaseAll); send.await.unwrap();
        assert!(read_message(&mut rx).await.is_err());
    }
    #[tokio::test] async fn truncated_and_oversized() {
        let (mut tx,mut rx)=tokio::io::duplex(64); tx.write_all(&[b'S',b'K',1,1,33,0,0,0]).await.unwrap(); assert!(read_message(&mut rx).await.is_err());
        let (mut tx,mut rx)=tokio::io::duplex(64); tx.write_all(&[b'S',b'K',1,1,32,0,0,0,1]).await.unwrap(); drop(tx); assert!(read_message(&mut rx).await.is_err());
    }
}
