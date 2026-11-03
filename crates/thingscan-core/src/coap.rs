//! Just enough CoAP (RFC 7252) to issue OCF discovery GETs and read their payloads.

pub const OPT_URI_PATH: u16 = 11;
pub const OPT_URI_QUERY: u16 = 15;
pub const OPT_ACCEPT: u16 = 17;
pub const OPT_BLOCK2: u16 = 23;
/// OCF-Accept-Content-Format-Version (OCF Core spec).
pub const OPT_OCF_ACCEPT_VERSION: u16 = 2049;

pub const FORMAT_OCF_CBOR: u16 = 10000;
/// OCF 1.0.0 encoded as major<<11 | minor<<6 | sub.
pub const OCF_VERSION_1_0: u16 = 0x0800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Confirmable = 0,
    NonConfirmable = 1,
    Ack = 2,
    Reset = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: Type,
    /// Code as class.detail, e.g. 0x45 == 2.05 Content.
    pub code: u8,
    pub message_id: u16,
    pub token: Vec<u8>,
    pub options: Vec<(u16, Vec<u8>)>,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn get(path: &str, query: Option<&str>, message_id: u16, token: &[u8]) -> Self {
        let mut options: Vec<(u16, Vec<u8>)> =
            path.split('/').filter(|s| !s.is_empty()).map(|seg| (OPT_URI_PATH, seg.as_bytes().to_vec())).collect();
        if let Some(q) = query {
            options.push((OPT_URI_QUERY, q.as_bytes().to_vec()));
        }
        Self { kind: Type::NonConfirmable, code: 0x01, message_id, token: token.to_vec(), options, payload: Vec::new() }
    }

    pub fn with_option(mut self, number: u16, value: Vec<u8>) -> Self {
        self.options.push((number, value));
        self
    }

    pub fn is_success(&self) -> bool {
        self.code >> 5 == 2
    }

    /// Block2 option (RFC 7959) as (block number, more follows, size exponent).
    pub fn block2(&self) -> Option<(u32, bool, u8)> {
        let (_, v) = self.options.iter().find(|(n, _)| *n == OPT_BLOCK2)?;
        let v = v.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32);
        Some((v >> 4, v & 0x08 != 0, (v & 0x07) as u8))
    }

    /// A copy of this request asking for block `num` of size 2^(szx+4).
    pub fn for_block(&self, message_id: u16, num: u32, szx: u8) -> Self {
        let mut options: Vec<_> = self.options.iter().filter(|(n, _)| *n != OPT_BLOCK2).cloned().collect();
        let v = (num << 4) | szx as u32;
        let bytes = v.to_be_bytes();
        let skip = if v < 0x100 {
            3
        } else if v < 0x10000 {
            2
        } else {
            1
        };
        options.push((OPT_BLOCK2, bytes[skip..].to_vec()));
        Self { message_id, options, ..self.clone() }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0x40 | ((self.kind as u8) << 4) | self.token.len() as u8, self.code];
        out.extend_from_slice(&self.message_id.to_be_bytes());
        out.extend_from_slice(&self.token);

        let mut options = self.options.clone();
        options.sort_by_key(|(n, _)| *n);
        let mut last = 0u16;
        for (number, value) in &options {
            let (delta_nibble, delta_ext) = nibble(number - last);
            let (len_nibble, len_ext) = nibble(value.len() as u16);
            out.push((delta_nibble << 4) | len_nibble);
            out.extend(delta_ext);
            out.extend(len_ext);
            out.extend_from_slice(value);
            last = *number;
        }
        if !self.payload.is_empty() {
            out.push(0xFF);
            out.extend_from_slice(&self.payload);
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 4 || buf[0] >> 6 != 1 {
            return None;
        }
        let kind = match (buf[0] >> 4) & 0x3 {
            0 => Type::Confirmable,
            1 => Type::NonConfirmable,
            2 => Type::Ack,
            _ => Type::Reset,
        };
        let tkl = (buf[0] & 0x0F) as usize;
        let code = buf[1];
        let message_id = u16::from_be_bytes([buf[2], buf[3]]);
        let token = buf.get(4..4 + tkl)?.to_vec();

        let mut i = 4 + tkl;
        let mut options = Vec::new();
        let mut number = 0u16;
        while i < buf.len() {
            if buf[i] == 0xFF {
                i += 1;
                break;
            }
            let byte = buf[i];
            i += 1;
            let delta = read_ext(byte >> 4, buf, &mut i)?;
            let len = read_ext(byte & 0x0F, buf, &mut i)? as usize;
            number = number.checked_add(delta)?;
            options.push((number, buf.get(i..i + len)?.to_vec()));
            i += len;
        }
        Some(Self { kind, code, message_id, token, options, payload: buf.get(i..).unwrap_or_default().to_vec() })
    }
}

pub fn uint_bytes(v: u16) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

fn nibble(v: u16) -> (u8, Vec<u8>) {
    match v {
        0..=12 => (v as u8, vec![]),
        13..=268 => (13, vec![(v - 13) as u8]),
        _ => (14, (v - 269).to_be_bytes().to_vec()),
    }
}

fn read_ext(nibble: u8, buf: &[u8], i: &mut usize) -> Option<u16> {
    match nibble {
        0..=12 => Some(nibble as u16),
        13 => {
            let v = *buf.get(*i)? as u16 + 13;
            *i += 1;
            Some(v)
        }
        14 => {
            let v = u16::from_be_bytes([*buf.get(*i)?, *buf.get(*i + 1)?]).checked_add(269)?;
            *i += 2;
            Some(v)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_oic_res_get() {
        let msg = Message::get("/oic/res", None, 0x1234, &[0xAB]);
        assert_eq!(msg.encode(), [0x51, 0x01, 0x12, 0x34, 0xAB, 0xB3, b'o', b'i', b'c', 0x03, b'r', b'e', b's']);
    }

    #[test]
    fn round_trips_extended_options() {
        let msg = Message::get("/oic/d", Some("rt=oic.wk.d"), 7, &[1, 2])
            .with_option(OPT_ACCEPT, uint_bytes(FORMAT_OCF_CBOR))
            .with_option(OPT_OCF_ACCEPT_VERSION, uint_bytes(OCF_VERSION_1_0));
        let mut sorted = msg.options.clone();
        sorted.sort_by_key(|(n, _)| *n);
        let decoded = Message::decode(&msg.encode()).unwrap();
        assert_eq!(decoded.options, sorted);
        assert_eq!(decoded.token, vec![1, 2]);
        assert_eq!(decoded.message_id, 7);
    }

    #[test]
    fn reads_block2_from_samsung_tv_reply() {
        // Header of a real /oic/res reply from a Samsung TV: Uri-Path echo, Content-Format 60,
        // Block2 (num 0, more, 1024-byte blocks), Size2 13367, then the first CBOR bytes.
        let buf = [
            0x51, 0x45, 0x12, 0x34, 0xAB, 0xB3, b'o', b'i', b'c', 0x03, b'r', b'e', b's', 0x11, 0x3C, 0xB1, 0x0E, 0x52,
            0x34, 0x37, 0xFF, 0x81, 0xBF,
        ];
        let msg = Message::decode(&buf).unwrap();
        assert!(msg.is_success());
        assert_eq!(msg.block2(), Some((0, true, 6)));
        assert_eq!(msg.payload, vec![0x81, 0xBF]);

        let next = Message::get("/oic/res", None, 1, &[0xAB]).for_block(2, 1, 6);
        assert_eq!(next.block2(), Some((1, false, 6)));
        assert!(next.encode().ends_with(&[0xC1, 0x16]));
        let far = next.for_block(3, 300, 6);
        assert_eq!(far.block2(), Some((300, false, 6)));
        assert_eq!(far.options.iter().filter(|(n, _)| *n == OPT_BLOCK2).count(), 1);
    }

    #[test]
    fn decodes_payload_and_content_format() {
        // 2.05 Content, NON, content-format 60 (application/cbor), payload 0xA0 (empty CBOR map)
        let buf = [0x50, 0x45, 0x00, 0x01, 0xC1, 60, 0xFF, 0xA0];
        let msg = Message::decode(&buf).unwrap();
        assert_eq!(msg.code, 0x45);
        assert_eq!(msg.options, vec![(12, vec![60])]);
        assert_eq!(msg.payload, vec![0xA0]);
    }
}
