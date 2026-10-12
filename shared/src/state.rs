use crate::{Bounds, Message};

#[derive(Default)]
pub struct SwitchState { ready: bool, epoch: u64, remote: bool }
impl SwitchState {
    pub fn set_ready(&mut self, ready: bool) { self.ready=ready; if !ready {self.remote=false;} }
    pub fn remote(&self) -> bool { self.remote }
    pub fn enter(&mut self, b: Bounds, x: i32, y: i32, dx: i32) -> Option<Message> {
        if !self.ready || self.remote || dx<=0 || x<b.right() {return None;}
        self.epoch=self.epoch.wrapping_add(1); self.remote=true;
        Some(Message::Enter{epoch:self.epoch,y:b.normalize_y(y)})
    }
    pub fn return_local(&mut self, epoch: u64) -> bool {
        if self.remote && epoch==self.epoch {self.remote=false;true} else {false}
    }
    pub fn release(&mut self) {self.remote=false;}
}

/// The mock and future native client clear this on every ownership/failure boundary.
pub struct ClientState {
    pub bounds: Bounds, pub x: i32, pub y: i32, pub epoch: Option<u64>,
    keys: [bool;256], buttons: [bool;5],
}
impl ClientState {
    pub fn new(bounds: Bounds) -> Self {Self{bounds,x:0,y:0,epoch:None,keys:[false;256],buttons:[false;5]}}
    pub fn release(&mut self) {self.epoch=None;self.keys.fill(false);self.buttons.fill(false);}
    pub fn held_count(&self) -> usize {self.keys.iter().filter(|v|**v).count()+self.buttons.iter().filter(|v|**v).count()}
    pub fn apply(&mut self, m: Message) -> Option<Message> {
        match m {
            Message::Enter{epoch,y} => {self.release();self.epoch=Some(epoch);self.x=0;self.y=self.bounds.denormalize_y(y);}
            Message::ReleaseAll => self.release(),
            Message::Move{dx,dy} if self.epoch.is_some() => {
                let x=i64::from(self.x)+i64::from(dx);
                self.y=(i64::from(self.y)+i64::from(dy)).clamp(i64::from(self.bounds.top),i64::from(self.bounds.top)+i64::from(self.bounds.height)-1) as i32;
                if x<0 { let r=Message::Return{epoch:self.epoch.unwrap(),y:self.bounds.normalize_y(self.y)}; self.release(); return Some(r); }
                self.x=x.min(i64::from(self.bounds.width)-1) as i32;
            }
            Message::Key{vk,down,..} if self.epoch.is_some() && vk<256 => self.keys[usize::from(vk)]=down,
            Message::Button{button,down} if self.epoch.is_some() && button<5 => self.buttons[usize::from(button)]=down,
            _ => {}
        }
        None
    }
}

#[cfg(test)] mod tests {
    use super::*;
    fn bounds()->Bounds{Bounds{left:0,top:-100,width:1920,height:1080}}
    #[test] fn authenticated_edge_and_stale_return() {
        let mut s=SwitchState::default(); assert!(s.enter(bounds(),1919,500,1).is_none());s.set_ready(true);
        assert!(s.enter(bounds(),1919,500,-1).is_none()); assert!(s.enter(bounds(),1918,500,1).is_none());
        assert!(s.enter(bounds(),1919,500,1).is_some()); assert!(!s.return_local(99));assert!(s.remote());assert!(s.return_local(1));
        assert!(s.enter(bounds(),1919,500,1).is_some());assert!(!s.return_local(1)); s.set_ready(false);assert!(!s.remote());
    }
    #[test] fn drag_return_and_disconnect_release() {
        let mut c=ClientState::new(bounds());c.apply(Message::Enter{epoch:1,y:32768});
        c.apply(Message::Key{vk:0xa2,scan:29,extended:false,down:true}); c.apply(Message::Button{button:0,down:true}); assert_eq!(c.held_count(),2);
        c.apply(Message::Move{dx:10,dy:i32::MAX});assert_eq!(c.y,979);
        assert!(matches!(c.apply(Message::Move{dx:-11,dy:0}),Some(Message::Return{epoch:1,..})));assert_eq!(c.held_count(),0);
        c.apply(Message::Enter{epoch:2,y:0});c.apply(Message::Button{button:1,down:true});c.release();assert_eq!(c.held_count(),0);assert_eq!(c.epoch,None);
    }
    #[test] fn normalization_endpoints() {let b=bounds();assert_eq!(b.normalize_y(-100),0);assert_eq!(b.normalize_y(979),65535);assert_eq!(b.denormalize_y(65535),979);}
}
