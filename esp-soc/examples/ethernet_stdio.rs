//! Local-only frame relay for the WASM ABI. Reuses the production VirtualNet/NAT;
//! guest TLS and application protocols stay entirely inside guest firmware.
//! Input: u64 guest microseconds, u32 frame count, repeated u16 length + frame.
//! Output: u32 frame count, repeated u16 length + frame. All fields little-endian.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener};

fn main() -> io::Result<()> {
    let mut net = esp_soc::net::VirtualNet::new(false);
    net.nat = Some(esp_soc::nat::Nat::new(false));
    // Opt-in loopback only. Existing outbound-only invocation is unchanged.
    let args: Vec<_> = std::env::args().skip(1).collect();
    let forward = match args.as_slice() {
        [] => None,
        [option, host_port, guest_port] if option == "--forward" => {
            let host = host_port.parse::<u16>().ok().filter(|p| *p != 0)
                .ok_or_else(|| io::Error::other("invalid forward host port"))?;
            let guest = guest_port.parse::<u16>().ok().filter(|p| *p != 0)
                .ok_or_else(|| io::Error::other("invalid forward guest port"))?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, host))?;
            listener.set_nonblocking(true)?;
            Some((listener, guest))
        }
        _ => return Err(io::Error::other("usage: ethernet_stdio [--forward HOST_PORT GUEST_PORT]")),
    };
    let mut guest_mac = None;
    let mut peer_port = 49152_u16;
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
            // Learn only the station's real source MAC from its emitted frames.
            guest_mac = Some(frame[6..12].try_into().unwrap());
            replies.extend(net.handle(&frame, now));
        }
        if let (Some((listener, guest_port)), Some(mac)) = (&forward, guest_mac) {
            let nat = net.nat.as_mut().unwrap();
            for _ in 0..4 {
                if nat.inbound_count() >= 4 { break; }
                match listener.accept() {
                    Ok((socket, peer)) => {
                        if !peer.ip().is_loopback() { continue; }
                        let target = esp_soc::nat::InboundTarget { mac, ip: net.sta_ip, port: *guest_port };
                        if let Some(syn) = nat.accept_inbound(target, peer_port, socket, now) { replies.push(syn); }
                        peer_port = if peer_port == u16::MAX { 49152 } else { peer_port + 1 };
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
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
