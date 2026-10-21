use super::*;
use std::net::TcpListener;

fn segment(flags: u8, data: &[u8]) -> Vec<u8> {
    let mut result = vec![0; 20];
    result[0..2].copy_from_slice(&1234u16.to_be_bytes());
    result[2..4].copy_from_slice(&80u16.to_be_bytes());
    result[4..8].copy_from_slice(&100u32.to_be_bytes());
    result[8..12].copy_from_slice(&200u32.to_be_bytes());
    result[12] = 0x50; result[13] = flags;
    result.extend_from_slice(data);
    result
}

#[test]
fn outage_resets_syn_and_existing_data_with_valid_sequence_and_checksums() {
    let mut net = VirtualNet::new(false);
    net.set_tcp_outage(true);
    for (flags, data, expected_seq, expected_ack, expected_flags) in [
        (2, &b""[..], 0, 101, 0x14),
        (0x18, &b"request"[..], 200, 0, 4),
        (1, &b"data"[..], 0, 105, 0x14),
    ] {
        let reply = net.ipv4(&ip_packet(6, &net.sta_ip, &[127,0,0,1], &segment(flags,data)), &[2;6]);
        assert_eq!(reply.len(),1);
        let tcp = &reply[0][34..];
        assert_eq!(u32::from_be_bytes(tcp[4..8].try_into().unwrap()),expected_seq);
        assert_eq!(u32::from_be_bytes(tcp[8..12].try_into().unwrap()),expected_ack);
        assert_eq!(tcp[13],expected_flags);
        assert_eq!(checksum(&reply[0][14..34],0),0);
        assert_eq!(transport_checksum(&[127,0,0,1],&net.sta_ip,6,tcp),0);
    }
    assert!(net.ipv4(&ip_packet(6,&net.sta_ip,&[127,0,0,1],&segment(4,b"")),&[2;6]).is_empty());
    assert_eq!(net.tcp_rejects,3,"a reset must not generate another reset");
    let mut bad = segment(2,b""); bad[12] = 0xf0;
    assert!(net.ipv4(&ip_packet(6,&net.sta_ip,&[127,0,0,1],&bad),&[2;6]).is_empty());
}

#[test]
fn outage_does_not_poison_resolver_and_restored_path_opens_real_host_socket() {
    let listener = TcpListener::bind(("127.0.0.1",0)).unwrap();
    let mut net = VirtualNet::new(false);
    net.nat = Some(crate::nat::Nat::new(false));
    net.nat.as_mut().unwrap().resolver = [127,0,0,1];
    let mut syn = segment(2,b"");
    syn[2..4].copy_from_slice(&listener.local_addr().unwrap().port().to_be_bytes());
    let request = ip_packet(6,&net.sta_ip,&[127,0,0,1],&syn);
    net.set_tcp_outage(true);
    assert_eq!(net.ipv4(&request,&[2;6]).len(),1);
    assert_eq!(net.nat.as_ref().unwrap().tcp_opened,0);
    net.set_tcp_outage(false);
    assert_eq!(net.nat.as_ref().unwrap().resolver,[127,0,0,1]);
    assert!(net.ipv4(&request,&[2;6]).is_empty());
    let deadline = std::time::Instant::now()+std::time::Duration::from_secs(2);
    while net.nat.as_ref().unwrap().tcp_opened == 0 {
        net.poll(0);
        assert!(std::time::Instant::now()<deadline,"restored host connection did not complete");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(net.nat.as_ref().unwrap().tcp_opened,1);
    let _peer = listener.accept().unwrap();
}
