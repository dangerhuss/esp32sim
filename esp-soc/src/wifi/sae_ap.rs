//! Group19/H2E initial-association peer. No firmware hooks or guest-time delays.
//! Protocol constants follow IDF6.1 wpa_supplicant/{common,crypto} and the reused
//! oxwifid SAE core (attribution and adaptations in sae/NOTICE).
// AES-CMAC adapted from oxwifid auth/crypto.rs, same pinned MIT source as sae/NOTICE.
use super::{sae, StaState, VirtualAp, WpaState, SAE_RSNXE, SAE_RSN_IE};
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

pub(super) struct Exchange {
    engine: sae::Sae,
    peer_commit: Vec<u8>,
    own_commit: Vec<u8>,
    own_confirm: Vec<u8>,
    peer_confirm: Option<Vec<u8>>,
    message2: Option<Vec<u8>>,
    message3: Option<Vec<u8>>,
    pub confirmed: bool,
}

pub(super) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any HMAC key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn prf(key: &[u8], context: &[u8]) -> [u8; 48] {
    let mut out = [0; 48];
    for counter in 1..=2u16 {
        let mut data = counter.to_le_bytes().to_vec();
        data.extend_from_slice(b"Pairwise key expansion");
        data.extend_from_slice(context);
        data.extend_from_slice(&384u16.to_le_bytes());
        let mut block = hmac_sha256(key, &data);
        let offset = (counter as usize - 1) * 32;
        let n = (48 - offset).min(32);
        out[offset..offset + n].copy_from_slice(&block[..n]);
        block.zeroize();
    }
    out
}

pub(super) fn ptk(
    pmk: &[u8],
    aa: &[u8; 6],
    spa: &[u8; 6],
    an: &[u8; 32],
    sn: &[u8; 32],
) -> [u8; 48] {
    let mut context = Vec::with_capacity(76);
    let (lo, hi) = if aa <= spa { (aa, spa) } else { (spa, aa) };
    context.extend_from_slice(lo);
    context.extend_from_slice(hi);
    let (lo, hi) = if an <= sn { (an, sn) } else { (sn, an) };
    context.extend_from_slice(lo);
    context.extend_from_slice(hi);
    prf(pmk, &context)
}

pub(super) fn aes_cmac(key: &[u8], msg: &[u8]) -> [u8; 16] {
    const RB: u8 = 0x87;
    fn shl1(b: &[u8; 16]) -> ([u8; 16], u8) {
        let mut out = [0u8; 16];
        let mut carry = 0u8;
        for i in (0..16).rev() {
            out[i] = (b[i] << 1) | carry;
            carry = b[i] >> 7;
        }
        (out, carry)
    }
    // Subkeys
    let l = esp_periph::crypto::aes_block(key, &[0u8; 16], false);
    let (mut k1, c1) = shl1(&l);
    if c1 != 0 {
        k1[15] ^= RB;
    }
    let (mut k2, c2) = shl1(&k1);
    if c2 != 0 {
        k2[15] ^= RB;
    }

    let n = msg.len().div_ceil(16).max(1);
    let complete = !msg.is_empty() && msg.len().is_multiple_of(16);

    let mut last = [0u8; 16];
    let last_start = (n - 1) * 16;
    if complete {
        last.copy_from_slice(&msg[last_start..]);
        for i in 0..16 {
            last[i] ^= k1[i];
        }
    } else {
        let rem = &msg[last_start..];
        last[..rem.len()].copy_from_slice(rem);
        last[rem.len()] = 0x80;
        for i in 0..16 {
            last[i] ^= k2[i];
        }
    }

    let mut x = [0u8; 16];
    for i in 0..n - 1 {
        let block = &msg[i * 16..i * 16 + 16];
        for j in 0..16 {
            x[j] ^= block[j];
        }
        x = esp_periph::crypto::aes_block(key, &x, false);
    }
    for j in 0..16 {
        x[j] ^= last[j];
    }
    esp_periph::crypto::aes_block(key, &x, false)
}

fn valid_mic(body: &[u8], key: &[u8]) -> bool {
    let mut data = body.to_vec();
    data[81..97].fill(0);
    sae::constant_time_eq(&aes_cmac(key, &data), &body[81..97])
}

impl VirtualAp {
    fn sae_reply(&mut self, sta: &[u8; 6], seq: u16, status: u16, payload: &[u8], at_us: u64) {
        let mut frame = self.hdr(11 << 4, sta, &self.cfg.bssid.clone());
        frame.extend_from_slice(&3u16.to_le_bytes());
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&status.to_le_bytes());
        frame.extend_from_slice(payload);
        // Coalesce pending auth retries. The initial-association peer owns at
        // most one exchange and a bounded number of air responses, even when
        // malformed input arrives without advancing the emulator clock.
        if self.queue.iter().any(|q| {
            q.frame.get(..2) == frame.get(..2)
                && q.frame.get(4..16) == frame.get(4..16)
                && q.frame.get(24..) == frame.get(24..)
        }) {
            return;
        }
        if self.queue.len() >= 32 {
            if self.log {
                eprintln!("[wifi] SAE response queue full; response refused");
            }
            return;
        }
        self.send(at_us, frame);
    }

    pub(super) fn on_sae_auth(
        &mut self,
        sta: &[u8; 6],
        seq: u16,
        status: u16,
        body: &[u8],
        now: u64,
    ) {
        if self.wpa.state == WpaState::Installed || body.len() > 256 {
            return;
        }
        if seq == 1 {
            // Initial peer deliberately supports only the hardware's H2E path.
            // Never silently downgrade a requested/unsupported SAE method.
            if status != 126 || body.len() < 98 {
                self.sae_reply(sta, 1, 1, &[], now + 300);
                return;
            }
            if body[..2] != 19u16.to_le_bytes() {
                self.sae_reply(sta, 1, 77, &body[..2], now + 300);
                return;
            }
            if let Some(exchange) = &self.sae_exchange {
                if self.sta == *sta && exchange.own_commit == body {
                    self.sae_reply(sta, 1, 1, &[], now + 300);
                    return;
                }
                if self.sta == *sta && exchange.peer_commit == body {
                    let (commit, confirm) =
                        (exchange.own_commit.clone(), exchange.own_confirm.clone());
                    self.sae_reply(sta, 1, 126, &commit, now + 300);
                    self.sae_reply(sta, 2, 0, &confirm, now + 1300);
                    return;
                }
            }
            let Some(password) = self.cfg.psk.as_ref() else {
                return;
            };
            let mut engine = sae::Sae::new_h2e(
                self.cfg.ssid.as_bytes(),
                password.as_bytes(),
                None,
                &self.cfg.bssid,
                sta,
            );
            // Like the existing WPA2 peer, AP keys are deterministic simulation
            // inputs. Guest entropy remains real. Domain-separate scalars and
            // bind them to this validated transcript, both addresses and SSID.
            let mut material = password.as_bytes().to_vec();
            material.extend_from_slice(self.cfg.ssid.as_bytes());
            material.extend_from_slice(&self.cfg.bssid);
            material.extend_from_slice(sta);
            material.extend_from_slice(body);
            let modulus = &engine.curve.n - BigUint::from(2u8);
            let mut scalars = Vec::new();
            for label in [b"AP-SAE-rand".as_slice(), b"AP-SAE-mask".as_slice()] {
                let mut hash = Sha256::new();
                hash.update(label);
                hash.update(&material);
                scalars
                    .push(BigUint::from_bytes_be(&hash.finalize()) % &modulus + BigUint::from(2u8));
            }
            material.zeroize();
            let mask = scalars.pop().expect("mask");
            let rand = scalars.pop().expect("rand");
            if (&mask + &rand) % &engine.curve.n <= BigUint::from(1u8) {
                return;
            }
            engine.prepare_commit((rand, mask));
            if engine.parse_peer_commit(body).is_err()
                || engine.is_reflection()
                || engine.process_commit().is_err()
            {
                self.sae_reply(sta, 1, 1, &[], now + 300);
                return;
            }
            let Ok(confirm) = engine.write_confirm() else {
                return;
            };
            let commit = engine.write_commit();
            self.sta = *sta;
            self.state = StaState::Idle;
            self.sae_exchange = Some(Exchange {
                engine,
                peer_commit: body.to_vec(),
                own_commit: commit.clone(),
                own_confirm: confirm.clone(),
                peer_confirm: None,
                message2: None,
                message3: None,
                confirmed: false,
            });
            self.sae_reply(sta, 1, 126, &commit, now + 300);
            self.sae_reply(sta, 2, 0, &confirm, now + 1300);
        } else if seq == 2 && status == 0 && body.len() == 34 && self.sta == *sta {
            let Some(exchange) = self.sae_exchange.as_mut() else {
                return;
            };
            if exchange.peer_confirm.as_deref() == Some(body) {
                return;
            }
            if exchange.engine.check_confirm(body).is_err() {
                return;
            }
            exchange.peer_confirm = Some(body.to_vec());
            exchange.confirmed = true;
            self.wpa.pmk.copy_from_slice(&exchange.engine.pmk);
            let mut seed = Sha256::new();
            seed.update(self.wpa.pmk);
            seed.update(self.cfg.bssid);
            seed.update(sta);
            self.wpa.anonce = seed.finalize().into();
            self.wpa
                .gtk
                .copy_from_slice(&hmac_sha256(&self.wpa.pmk, b"AP GTK")[..16]);
            self.wpa
                .igtk
                .copy_from_slice(&hmac_sha256(&self.wpa.pmk, b"AP IGTK")[..16]);
            self.state = StaState::Authenticated;
            if self.log {
                eprintln!("[wifi] SAE H2E commit and peer confirm verified (group19)");
            }
        }
    }

    pub(super) fn on_sae_eapol(&mut self, data: &[u8], now: u64) {
        if data.len() < 99 || !matches!(data[0], 1..=3) || data[1] != 3 || data[4] != 2 {
            return;
        }
        let n = 4 + u16::from_be_bytes([data[2], data[3]]) as usize;
        let Some(body) = data.get(..n).filter(|b| b.len() >= 99 && b.len() <= 512) else {
            return;
        };
        if 99 + u16::from_be_bytes([body[97], body[98]]) as usize != body.len() {
            return;
        }
        let Some(exchange) = self.sae_exchange.as_ref().filter(|s| s.confirmed) else {
            return;
        };
        if exchange.message2.as_deref() == Some(body)
            && self.wpa.state == WpaState::AwaitingMessage4
        {
            if let Some(frame) = exchange.message3.clone() {
                if !self.queue.iter().any(|queued| queued.frame == frame) {
                    self.send(now + 2000, frame);
                }
            }
            return;
        }
        let info = u16::from_be_bytes([body[5], body[6]]);
        let replay = u64::from_be_bytes(body[9..17].try_into().expect("fixed replay"));
        if replay != self.wpa.replay {
            return;
        }
        if info == 0x0108 && self.wpa.state == WpaState::AwaitingMessage2 {
            let mut snonce = [0; 32];
            snonce.copy_from_slice(&body[17..49]);
            let mut keys = ptk(
                &self.wpa.pmk,
                &self.cfg.bssid,
                &self.sta,
                &self.wpa.anonce,
                &snonce,
            );
            if !valid_mic(body, &keys[..16]) {
                keys.zeroize();
                return;
            }
            let mut kd = SAE_RSN_IE.to_vec();
            kd.extend_from_slice(SAE_RSNXE);
            kd.extend_from_slice(&[221, 22, 0, 15, 172, 1, 1, 0]);
            kd.extend_from_slice(&self.wpa.gtk);
            kd.extend_from_slice(&[221, 28, 0, 15, 172, 9, 4, 0, 0, 0, 0, 0, 0, 0]);
            kd.extend_from_slice(&self.wpa.igtk);
            if !kd.len().is_multiple_of(8) {
                kd.push(221);
                while !kd.len().is_multiple_of(8) {
                    kd.push(0);
                }
            }
            let kek: [u8; 16] = keys[16..32].try_into().expect("KEK");
            let wrapped = esp_periph::crypto::aes_key_wrap(&kek, &kd);
            self.wpa.kck.copy_from_slice(&keys[..16]);
            self.wpa.replay += 1;
            let m3 = self.eapol(0x13c8, self.wpa.anonce, &wrapped, Some(&keys[..16]));
            keys.zeroize();
            kd.zeroize();
            let exchange = self.sae_exchange.as_mut().expect("confirmed exchange");
            exchange.message2 = Some(body.to_vec());
            exchange.message3 = Some(m3.clone());
            self.send(now + 2000, m3);
            self.wpa.state = WpaState::AwaitingMessage4;
        } else if info == 0x0308
            && self.wpa.state == WpaState::AwaitingMessage4
            && valid_mic(body, &self.wpa.kck)
        {
            self.wpa.state = WpaState::Installed;
            if self.log {
                eprintln!("[wifi] SAE four-way handshake AES-CMAC and replay verified");
            }
        }
    }
}
