//! Local-only frame relay for the WASM ABI. Reuses the production VirtualNet/NAT;
//! guest TLS and application protocols stay entirely inside guest firmware.
//! Input: u64 guest microseconds, u32 frame count, repeated u16 length + frame.
//! Output: u32 frame count, repeated u16 length + frame. All fields little-endian.
use std::io::{self, Read, Write};

fn main() -> io::Result<()> {
    let mut net = esp_soc::net::VirtualNet::new(false);
    net.nat = Some(esp_soc::nat::Nat::new(false));
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut header = [0; 12];
        match input.read_exact(&mut header[..1]) {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            result => result?,
        }
        input.read_exact(&mut header[1..])?;
        let now = u64::from_le_bytes(header[..8].try_into().unwrap());
        let count = u32::from_le_bytes(header[8..].try_into().unwrap());
        if count > 64 { return Err(io::Error::other("too many input frames")); }
        let mut replies = Vec::new();
        for _ in 0..count {
            let mut length = [0; 2];
            input.read_exact(&mut length)?;
            let length = usize::from(u16::from_le_bytes(length));
            if !(14..=1518).contains(&length) { return Err(io::Error::other("invalid frame length")); }
            let mut frame = vec![0; length];
            input.read_exact(&mut frame)?;
            replies.extend(net.handle(&frame, now));
        }
        replies.extend(net.poll(now));
        if replies.len() > 256 { return Err(io::Error::other("too many reply frames")); }
        output.write_all(&(replies.len() as u32).to_le_bytes())?;
        for frame in replies {
            if !(14..=1518).contains(&frame.len()) { return Err(io::Error::other("invalid reply length")); }
            output.write_all(&(frame.len() as u16).to_le_bytes())?;
            output.write_all(&frame)?;
        }
        output.flush()?;
    }
}
