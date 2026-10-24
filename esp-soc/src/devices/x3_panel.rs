//! X3 UC8253 B/W protocol boundary, from Crusty's board_adapters/x3_display.rs.
//! 792x528 visible pixels, bottom-first rows, GPIO4 DC/5 reset/6 BUSY.
//! Nominal BUSY periods are functional fixtures, not calibrated silicon/optics.
use crate::board::BoardEdge;

const BYTES: usize = 52_272;
const HZ: u64 = 160_000_000;
pub struct X3Panel {
    old: Vec<u8>, new: Vec<u8>, shown: Vec<u8>,
    command: u8, position: usize, old_valid: bool, new_valid: bool,
    enabled: u64, output: u64, cycle: u64, deadline: Option<u64>,
    powered: bool, refreshing: bool, full: bool, luts: u8,
    pub frames: u64, pub faults: u64, current_invalid: bool, edges: Vec<BoardEdge>,
}
impl Default for X3Panel { fn default() -> Self { Self::new() } }
impl X3Panel {
    pub fn new() -> Self { Self {
        old: vec![255;BYTES],new: vec![255;BYTES],shown: vec![255;BYTES],
        command:0,position:0,old_valid:false,new_valid:false,enabled:0,output:0,
        cycle:0,deadline:None,powered:false,refreshing:false,full:true,luts:0,
        frames:0,faults:0,current_invalid:false,edges:Vec::new(),
    }}
    // Fault history is diagnostic; only the actual reset pin clears active refusal.
    pub(super) fn fault(&mut self) { self.faults+=1; self.current_invalid=true; }
    fn busy(&mut self, us:u64, refresh:bool) {
        self.deadline=Some(self.cycle.saturating_add(us*HZ/1_000_000));
        self.refreshing=refresh;self.edges.push(BoardEdge{cycle:self.cycle,pin:6,level:false});
    }
    pub fn gpio(&mut self, cycle:u64, enabled:u64, output:u64) {
        self.advance(cycle);
        if enabled&(1<<5)!=0 && output&(1<<5)==0 && (self.enabled&(1<<5)==0 || self.output&(1<<5)!=0) {
            self.current_invalid=false;
            self.command=0;self.position=0;self.old_valid=false;self.new_valid=false;
            self.powered=false;self.refreshing=false;self.deadline=None;self.luts=0;
            self.edges.push(BoardEdge{cycle,pin:6,level:true});
        }
        self.enabled=enabled;self.output=output;
    }
    pub fn transfer(&mut self, tx:&[u8]) {
        if self.enabled & self.output & (1<<5)==0 { return; }
        if self.deadline.is_some() { self.fault();return; }
        if self.output&(1<<4)!=0 { self.data(tx); }
        else { for &byte in tx { self.command(byte); } }
    }
    fn command(&mut self, command:u8) {
        if matches!(self.command,0x10|0x13) && self.position!=BYTES { self.fault(); }
        self.command=command;self.position=0;
        match command {
            0x10=>self.old_valid=false,0x13=>self.new_valid=false,
            4=>{self.powered=true;self.busy(2_000,false);},
            2=>{self.powered=false;self.busy(2_000,false);},
            0x12=>{
                if !self.powered || !self.old_valid || !self.new_valid || self.luts!=31
                    || (!self.full && self.old!=self.shown) || self.current_invalid {
                    self.fault();return;
                }
                self.busy(if self.full {600_000}else{150_000},true);
            },
            _=>{}
        }
    }
    fn data(&mut self, tx:&[u8]) {
        if matches!(self.command,0x10|0x13) {
            if tx.len()>BYTES.saturating_sub(self.position) {self.fault();return;}
            let plane=if self.command==0x10 {&mut self.old}else{&mut self.new};
            plane[self.position..self.position+tx.len()].copy_from_slice(tx);self.position+=tx.len();
            if self.position==BYTES {if self.command==0x10 {self.old_valid=true;}else{self.new_valid=true;}}
        } else if (0x20..=0x24).contains(&self.command) {
            if tx.len()!=42 {self.fault();return;}
            if self.command==0x20 {self.full=tx[1]==0x18;self.luts=0;}
            self.luts|=1<<(self.command-0x20);
        }
    }
    pub fn advance(&mut self, cycle:u64) {
        self.cycle=cycle;
        if self.deadline.is_some_and(|d|cycle>=d) {
            if self.refreshing {self.shown.copy_from_slice(&self.new);self.frames+=1;}
            self.refreshing=false;
            self.edges.push(BoardEdge{cycle:self.deadline.take().unwrap(),pin:6,level:true});
        }
    }
    pub fn deadline(&self)->Option<u64>{self.deadline}
    pub fn edges(&mut self)->Vec<BoardEdge>{std::mem::take(&mut self.edges)}
    pub fn idle(&self)->bool{self.deadline.is_none()}
    pub fn display(&self)->(u32,u32,Vec<u16>,u64){
        let mut pixels=Vec::with_capacity(792*528);
        for row in self.shown.as_chunks::<99>().0.iter().rev() {
            for &byte in row {for bit in 0..8 {pixels.push(if byte&(128>>bit)!=0 {65535}else{0});}}
        }
        (792,528,pixels,self.frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->X3Panel{let mut p=X3Panel::new();p.gpio(0,(1<<4)|(1<<5),1<<5);p}
    fn cmd(p:&mut X3Panel,c:u8){p.gpio(p.cycle,(1<<4)|(1<<5),1<<5);p.transfer(&[c]);}
    fn data(p:&mut X3Panel,b:&[u8]){p.gpio(p.cycle,(1<<4)|(1<<5),(1<<4)|(1<<5));p.transfer(b);}
    fn plane(p:&mut X3Panel,c:u8,b:u8){cmd(p,c);data(p,&vec![b;BYTES]);cmd(p,0x11);}
    fn bank(p:&mut X3Panel,full:bool){for c in 0x20..=0x24{cmd(p,c);let mut b=[0;42];b[1]=if full{0x18}else{6};data(p,&b);}}
    #[test]
    fn real_planes_publish_only_after_busy_and_require_confirmed_differential_baseline(){
        let mut p=fixture();plane(&mut p,0x10,255);plane(&mut p,0x13,0);bank(&mut p,true);
        cmd(&mut p,4);p.advance(320_000);cmd(&mut p,0x12);
        assert_eq!(p.frames,0);assert_eq!(p.display().2[0],65535);
        p.advance(p.deadline().unwrap());assert_eq!(p.frames,1);assert_eq!(p.display().2[0],0);
        // The old plane is still white. A new partial must not pass without sync.
        bank(&mut p,false);cmd(&mut p,0x12);assert_eq!(p.frames,1);assert_eq!(p.faults,1);
    }
    #[test]
    fn busy_does_not_publish_early_and_reset_invalidates_controller_custody(){
        let mut p=fixture();plane(&mut p,0x10,255);plane(&mut p,0x13,0xaa);bank(&mut p,true);
        cmd(&mut p,4);p.advance(320_000);cmd(&mut p,0x12);let deadline=p.deadline().unwrap();
        p.advance(deadline-1);assert_eq!(p.frames,0);p.advance(deadline);assert_eq!(p.frames,1);
        p.gpio(deadline+1,1<<5,0);assert!(!p.old_valid && !p.new_valid);assert_eq!(p.frames,1);
    }
}
