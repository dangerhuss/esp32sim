// SPDX-License-Identifier: MIT
// J.10 controls adapted from pinned oxwifid tests/sae_vectors.rs; see sae/NOTICE.
//! Verify the SAE implementation against the IEEE 802.11-2020 Annex J.10 test
//! vectors (the same ones reference implementation's `sae_tests()` uses).
//!
//!   * H2E PWE (group 19) is checked directly against `pwe_19_x/y`.
//!   * The SAE protocol (commit, shared secret k, KCK/PMK/PMKID) is checked
//!     against the J.10 commit/key vector by recovering its PWE from the
//!     committed element and the known `mask` (so the hunting-and-pecking PWE
//!     derivation does not need to be reimplemented).

use super::sae::{self, Curve, Point, Sae};
fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn to_hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
use num_bigint::BigUint;

// ---- Annex J.10 constants -------------------------------------------------

const ADDR1: [u8; 6] = [0x4d, 0x3f, 0x2f, 0xff, 0xe3, 0x87];
const ADDR2: [u8; 6] = [0xa5, 0xd8, 0xaa, 0x95, 0x8e, 0x3c];
const ADDR1B: [u8; 6] = [0x00, 0x09, 0x5b, 0x66, 0xec, 0x1e];
const ADDR2B: [u8; 6] = [0x00, 0x0b, 0x6b, 0xd9, 0x02, 0x46];

const LOCAL_RAND: &str = "992465fd3daa3c60aa6565b7f62a2a7f2e12dd12f198faf4fbed89d7ff1ace94";
const LOCAL_MASK: &str = "9507a90f777a044d6a0830b91ea3d5dd70bece44e1acffb86983b5e1bf9fb322";
const LOCAL_COMMIT_FULL: &str = "13002e2c0f0db52440ad146d967114ce005ce1eab0aa2c2e5c2871b774f6c2575c65d5ad9e00829707aa36ba8b859738fc961d08243505f47c035376d7ac4bc8d7b95083bf43827d0fc31ed778dd3671fd21a46d1091d64b6f9a1e1272621325dbe1";
const PEER_COMMIT: &str = "1300591b96f3397fb945100848e7b550543b6720d88337ee93fc49fd6df7e08b5223e71b9bb048d3873f20556953a96c91536fd8ee6ca9b4a68a148b056a909be03e83ae208f60f8ef5537858074db06687032399862999b511e0a1552a5fea317c2";
const KCK: &str = "1e733f6d9bd53256287304338831b09a39406d121017073a5c30db36f36cb81a";
const PMK: &str = "4e4dfab1a2dd8ac1a91790f953faaa452ae5c6873ab75b63605ba663f8a7fe59";
const PMKID: &str = "8747a600eea3f9f22475df58ca1e5498";
const PWE19_X: &str = "c93049b9e64000f848201649e999f2b5c22dea69b5632c9df4d633b8aa1f6c1e";
const PWE19_Y: &str = "73634e94b53d82e7383a8d258199d9dc1a5ee8269d060382ccbf33e614ff59a0";

#[test]
fn h2e_pwe_group19_matches_ieee_j10() {
    let c = Curve::p256();
    let pt = sae::derive_pt(&c, b"byteme", b"mekmitasdigoat", Some(b"psk4internet"));
    let pwe = sae::derive_pwe_from_pt(&c, &pt, &ADDR1B, &ADDR2B);
    let bin = c.point_to_bin(&pwe).unwrap();
    assert_eq!(to_hex(&bin[..32]), PWE19_X, "PWE.x");
    assert_eq!(to_hex(&bin[32..]), PWE19_Y, "PWE.y");
}

/// Recover the hunting-and-pecking PWE used by the J.10 commit vector from the
/// committed element and the known mask: element = -(mask * PWE), so
/// PWE = mask^-1 * (-element).
fn recover_pwe(c: &Curve, commit_full: &[u8], mask: &BigUint) -> Point {
    let elem = c.point_from_bin(&commit_full[34..98]).unwrap();
    let neg_elem = c.negate(&elem);
    let inv = sae::mod_inverse(mask, &c.n);
    c.scalar_mul(&inv, &neg_elem)
}

#[test]
fn hunting_and_pecking_matches_ieee_j10() {
    // The J.10 commit/key vector is generated with the hunting-and-pecking PWE.
    // Deriving it independently and running the protocol must reproduce the
    // standard's local commit, KCK, PMK, and PMKID.
    let c = Curve::p256();
    let pwe =
        sae::derive_pwe_hunting_pecking(&c, b"mekmitasdigoat", &ADDR1, &ADDR2).expect("H&P PWE");
    assert!(c.on_curve(&pwe));

    let rand = BigUint::from_bytes_be(&from_hex(LOCAL_RAND));
    let mask = BigUint::from_bytes_be(&from_hex(LOCAL_MASK));
    let mut sae = Sae::with_pwe(pwe);
    sae.prepare_commit((rand, mask));
    assert_eq!(
        to_hex(&sae.write_commit()),
        LOCAL_COMMIT_FULL,
        "commit (H&P PWE)"
    );

    sae.parse_peer_commit(&from_hex(PEER_COMMIT)).unwrap();
    sae.process_commit().unwrap();
    assert_eq!(to_hex(&sae.kck), KCK, "KCK");
    assert_eq!(to_hex(&sae.pmk), PMK, "PMK");
    assert_eq!(to_hex(&sae.pmkid), PMKID, "PMKID");
}

#[test]
fn sae_protocol_matches_ieee_j10() {
    let c = Curve::p256();
    let local_commit = from_hex(LOCAL_COMMIT_FULL);
    let peer_commit = from_hex(PEER_COMMIT);
    let rand = BigUint::from_bytes_be(&from_hex(LOCAL_RAND));
    let mask = BigUint::from_bytes_be(&from_hex(LOCAL_MASK));

    let pwe = recover_pwe(&c, &local_commit, &mask);
    assert!(c.on_curve(&pwe), "recovered PWE must be on curve");

    let mut sae = Sae::with_pwe(pwe);
    sae.prepare_commit((rand, mask));

    // The reconstructed commit must equal the J.10 local commit byte-for-byte.
    assert_eq!(to_hex(&sae.write_commit()), to_hex(&local_commit), "commit");

    sae.parse_peer_commit(&peer_commit)
        .expect("peer commit parses");
    sae.process_commit().expect("process commit");

    assert_eq!(to_hex(&sae.kck), KCK, "KCK");
    assert_eq!(to_hex(&sae.pmk), PMK, "PMK");
    assert_eq!(to_hex(&sae.pmkid), PMKID, "PMKID");
}

use super::{ApConfig, StaState, VirtualAp, WpaState, SAE_RSN_IE};
const STA: [u8; 6] = [2, 3, 4, 5, 6, 7];
fn config() -> ApConfig {
    ApConfig::parse("ssid=fixture,psk=correct-password,auth=sae").unwrap()
}
fn frame(fc: u16, body: &[u8]) -> Vec<u8> {
    let mut f = vec![0; 24];
    f[..2].copy_from_slice(&fc.to_le_bytes());
    f[4..10].copy_from_slice(&config().bssid);
    f[10..16].copy_from_slice(&STA);
    f[16..22].copy_from_slice(&config().bssid);
    f.extend_from_slice(body);
    f
}
fn auth(ap: &mut VirtualAp, seq: u16, status: u16, body: &[u8]) {
    let mut b = 3u16.to_le_bytes().to_vec();
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(&status.to_le_bytes());
    b.extend_from_slice(body);
    ap.on_station_tx(&frame(11 << 4, &b), 0);
}
fn station(password: &[u8]) -> Sae {
    let mut s = Sae::new_h2e(b"fixture", password, None, &STA, &config().bssid);
    s.prepare_commit((BigUint::from(5u8), BigUint::from(7u8)));
    s
}
fn exchanged(password: &[u8]) -> (VirtualAp, Sae) {
    let mut ap = VirtualAp::new(config(), false);
    let mut sta = station(password);
    auth(&mut ap, 1, 126, &sta.write_commit());
    assert_eq!(ap.state, StaState::Idle);
    assert_eq!(ap.queue.len(), 2);
    sta.parse_peer_commit(&ap.queue[0].frame[30..]).unwrap();
    sta.process_commit().unwrap();
    (ap, sta)
}
fn associated() -> (VirtualAp, [u8; 48]) {
    let (mut ap, mut sta) = exchanged(b"correct-password");
    sta.check_confirm(&ap.queue[1].frame[30..]).unwrap();
    auth(&mut ap, 2, 0, &sta.write_confirm().unwrap());
    assert_eq!(ap.state, StaState::Authenticated);
    assert_eq!(ap.wpa.pmk.as_slice(), sta.pmk);
    ap.queue.clear();
    let mut body = vec![1, 0, 0, 0];
    body.extend_from_slice(SAE_RSN_IE);
    ap.on_station_tx(&frame(0, &body), 0);
    assert_eq!(ap.state, StaState::Associated);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage2);
    let snonce = [0x5a; 32];
    let keys = super::sae_ap::ptk(&sta.pmk, &config().bssid, &STA, &ap.wpa.anonce, &snonce);
    ap.queue.clear();
    (ap, keys)
}
fn eapol(info: u16, replay: u64, keys: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 99];
    b[0] = 2;
    b[1] = 3;
    b[2..4].copy_from_slice(&95u16.to_be_bytes());
    b[4] = 2;
    b[5..7].copy_from_slice(&info.to_be_bytes());
    b[9..17].copy_from_slice(&replay.to_be_bytes());
    if info == 0x0108 {
        b[17..49].fill(0x5a);
    }
    let mic = super::sae_ap::aes_cmac(&keys[..16], &b);
    b[81..97].copy_from_slice(&mic);
    b
}
fn send_eapol(ap: &mut VirtualAp, b: &[u8]) {
    let mut data = vec![170, 170, 3, 0, 0, 0, 136, 142];
    data.extend_from_slice(b);
    ap.on_station_tx(&frame(0x0108, &data), 0);
}
#[test]
fn aes_cmac_matches_rfc4493_not_sae_ext_key_hmac() {
    let key = from_hex("2b7e151628aed2a6abf7158809cf4f3c");
    assert_eq!(
        to_hex(&super::sae_ap::aes_cmac(&key, &[])),
        "bb1d6929e95937287fa37d129b756746"
    );
    assert_eq!(
        to_hex(&super::sae_ap::aes_cmac(
            &key,
            &from_hex("6bc1bee22e409f96e93d7e117393172a")
        )),
        "070a16b46b4d4144f79bdd9dd04a287c"
    );
}
#[test]
fn sae_wrong_password_never_authenticates_or_associates() {
    let (mut ap, mut sta) = exchanged(b"incorrect-password");
    assert!(sta.check_confirm(&ap.queue[1].frame[30..]).is_err());
    auth(&mut ap, 2, 0, &sta.write_confirm().unwrap());
    assert_eq!(ap.state, StaState::Idle);
    assert_eq!(ap.wpa.pmk, [0; 32]);
    let mut body = vec![1, 0, 0, 0];
    body.extend_from_slice(SAE_RSN_IE);
    ap.on_station_tx(&frame(0, &body), 0);
    assert_eq!(ap.state, StaState::Idle);
}
#[test]
fn sae_commit_retries_are_identical_and_reflection_is_rejected() {
    let (mut ap, sta) = exchanged(b"correct-password");
    let commit = ap.queue[0].frame[30..].to_vec();
    let confirm = ap.queue[1].frame[30..].to_vec();
    ap.queue.clear();
    auth(&mut ap, 1, 126, &sta.write_commit());
    assert_eq!(ap.queue.len(), 2);
    assert_eq!(ap.queue[0].frame[30..], commit);
    assert_eq!(ap.queue[1].frame[30..], confirm);
    ap.queue.clear();
    auth(&mut ap, 1, 126, &commit);
    assert_eq!(ap.queue.len(), 1);
    assert_eq!(&ap.queue[0].frame[28..30], &1u16.to_le_bytes());
    assert_eq!(ap.state, StaState::Idle);
}
#[test]
fn sae_truncated_unknown_group_and_bad_curve_inputs_fail_closed() {
    let commit = station(b"correct-password").write_commit();
    for len in 0..98 {
        let mut ap = VirtualAp::new(config(), false);
        auth(&mut ap, 1, 126, &commit[..len]);
        assert_eq!(ap.state, StaState::Idle);
    }
    for field in [0, 2, 34] {
        let mut ap = VirtualAp::new(config(), false);
        let mut b = commit.clone();
        if field == 0 {
            b[0] = 20;
        } else if field == 2 {
            b[2..34].fill(0);
        } else {
            b[34..].fill(0);
        }
        auth(&mut ap, 1, 126, &b);
        assert_eq!(ap.state, StaState::Idle);
        assert!(ap.sae_exchange.is_none());
    }
    let mut ap = VirtualAp::new(config(), false);
    auth(&mut ap, 1, 0, &commit);
    assert!(ap.sae_exchange.is_none());
}
#[test]
fn sae_four_way_checks_mic_replay_and_exact_retries() {
    let (mut ap, keys) = associated();
    let m2 = eapol(0x0108, 1, &keys);
    let mut bad = m2.clone();
    bad[81] ^= 1;
    send_eapol(&mut ap, &bad);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage2);
    assert!(ap.queue.is_empty());
    send_eapol(&mut ap, &m2);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage4);
    assert_eq!(ap.queue.len(), 1);
    let m3 = ap.queue[0].frame.clone();
    let mut b = m3[32..].to_vec();
    let mic = b[81..97].to_vec();
    b[81..97].fill(0);
    assert_eq!(mic, super::sae_ap::aes_cmac(&keys[..16], &b));
    assert_eq!(&b[5..7], &0x13c8u16.to_be_bytes());
    ap.queue.clear();
    send_eapol(&mut ap, &m2);
    assert_eq!(ap.queue[0].frame, m3);
    assert_eq!(ap.wpa.replay, 2);
    for _ in 0..100 { send_eapol(&mut ap,&m2); }
    assert_eq!(ap.queue.len(),1,"pending message3 retries coalesce");
    for replay in [0, 1, 3] {
        send_eapol(&mut ap, &eapol(0x0308, replay, &keys));
        assert_eq!(ap.wpa.state, WpaState::AwaitingMessage4);
    }
    let mut m4 = eapol(0x0308, 2, &keys);
    m4[81] ^= 1;
    send_eapol(&mut ap, &m4);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage4);
    m4[81] ^= 1;
    send_eapol(&mut ap, &m4);
    assert_eq!(ap.wpa.state, WpaState::Installed);
    ap.on_station_tx(&frame(12 << 4, &[0, 0]), 0);
    assert_eq!(
        ap.state,
        StaState::Associated,
        "unverified PMF teardown refused"
    );
}
#[test]
fn sae_runtime_config_and_capabilities_are_explicit() {
    assert!(ApConfig::parse("auth=sae").is_err());
    assert!(ApConfig::parse("auth=wpa2").is_err());
    assert!(ApConfig::parse("auth=saee,psk=correct-password").is_err());
    let mut ap = VirtualAp::new(config(), false);
    let beacon = ap.beacon_like(8, &[255; 6], 0);
    assert!(super::ies(&beacon, 36)
        .iter()
        .any(|(id, b)| *id == 48 && *b == &SAE_RSN_IE[2..]));
    assert!(super::ies(&beacon, 36)
        .iter()
        .any(|(id, b)| *id == 244 && *b == [32]));
}
#[test]
fn dma_checksum_trailer_is_removed_once_at_the_radio_boundary() {
    for trailer in [[239, 190, 173, 222], [1, 2, 3, 4], [0, 0, 0, 0]] {
        let mut link = super::StationLink::default();
        link.attach(Some(VirtualAp::new(config(), false)), None);
        let mut body = vec![3, 0, 1, 0, 126, 0];
        body.extend_from_slice(&station(b"correct-password").write_commit());
        let mut dma = frame(11 << 4, &body);
        dma.extend_from_slice(&trailer);
        link.station_tx_with_fcs(&dma, 0);
        assert_eq!(link.ap().unwrap().queue.len(), 2);
        assert_eq!(link.ap().unwrap().queue[0].frame.len(), 128);
    }
    let mut link = super::StationLink::default();
    link.station_tx_with_fcs(&[0; 3], 0);
    assert_eq!(link.tx_dropped(), 1);
}
#[test]
fn authenticated_session_refuses_unverified_reassociation() {
    let (mut ap, keys) = associated();
    send_eapol(&mut ap, &eapol(0x0108, 1, &keys));
    send_eapol(&mut ap, &eapol(0x0308, 2, &keys));
    assert_eq!(ap.wpa.state, WpaState::Installed);
    ap.queue.clear();
    let mut b = vec![1, 0, 0, 0];
    b.extend_from_slice(SAE_RSN_IE);
    ap.on_station_tx(&frame(0, &b), 0);
    assert_eq!(ap.wpa.state, WpaState::Installed);
    assert!(ap.queue.is_empty());
}
#[test]
fn pending_sae_retries_and_hostile_air_responses_are_bounded() {
    let mut ap = VirtualAp::new(config(), false);
    let commit = station(b"correct-password").write_commit();
    for _ in 0..100 {
        auth(&mut ap, 1, 126, &commit);
    }
    assert_eq!(ap.queue.len(), 2);
    for i in 0..100u8 {
        let mut f = frame(11 << 4, &[3, 0, 1, 0, 126, 0]);
        f[10] = i;
        ap.on_station_tx(&f, 0);
    }
    assert_eq!(ap.queue.len(), 32);
}
