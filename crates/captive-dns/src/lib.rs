//! A DNS server that answers every name with one address: that of the access point a device
//! opens to be set up. A phone that joins the access point then opens the setup page by itself,
//! as it does for the sign-in page of a public network.

use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};

const PORT: u16 = 53;
const LIFETIME_SECONDS: u32 = 60;
const LONGEST_QUESTION: usize = 512;
const ADDRESS: u16 = 1;
const ANY: u16 = 255;
const INTERNET: u16 = 1;

/// The answer to a standard query with one question: `address` for a question about an IPv4
/// address, and no record for any other.
fn answer(query: &[u8], address: Ipv4Addr) -> Option<Vec<u8>> {
    let flags = u16::from_be_bytes([*query.get(2)?, *query.get(3)?]);
    let is_response = flags & 0x8000 != 0;
    let opcode = (flags >> 11) & 0x0f;
    let questions = u16::from_be_bytes([*query.get(4)?, *query.get(5)?]);
    if is_response || opcode != 0 || questions != 1 {
        return None;
    }
    let mut offset = 12;
    loop {
        let length = usize::from(*query.get(offset)?);
        offset += 1;
        if length == 0 {
            break;
        }
        if length & 0xc0 != 0 {
            return None;
        }
        offset += length;
    }
    let question_end = offset + 4;
    let question = query.get(12..question_end)?;
    let kind = u16::from_be_bytes([query[offset], query[offset + 1]]);
    let class = u16::from_be_bytes([query[offset + 2], query[offset + 3]]) & 0x7fff;
    let answers = u16::from((kind == ADDRESS || kind == ANY) && class == INTERNET);

    let recursion_desired = flags & 0x0100;
    let mut response = Vec::with_capacity(question_end + 16);
    response.extend_from_slice(&query[..2]);
    response.extend_from_slice(&(0x8480 | recursion_desired).to_be_bytes());
    response.extend_from_slice(&1u16.to_be_bytes());
    response.extend_from_slice(&answers.to_be_bytes());
    response.extend_from_slice(&[0, 0, 0, 0]);
    response.extend_from_slice(question);
    if answers == 1 {
        response.extend_from_slice(&[0xc0, 0x0c]);
        response.extend_from_slice(&ADDRESS.to_be_bytes());
        response.extend_from_slice(&INTERNET.to_be_bytes());
        response.extend_from_slice(&LIFETIME_SECONDS.to_be_bytes());
        response.extend_from_slice(&4u16.to_be_bytes());
        response.extend_from_slice(&address.octets());
    }
    Some(response)
}

/// Answers the questions that reach `address` with `address`, for as long as the device runs.
pub fn serve(address: Ipv4Addr) -> io::Result<Infallible> {
    let socket = UdpSocket::bind((address, PORT))?;
    let mut question = [0; LONGEST_QUESTION];
    loop {
        let (length, asker) = socket.recv_from(&mut question)?;
        if let Some(answer) = answer(&question[..length], address) {
            // An answer that does not leave is asked for again.
            let _ = socket.send_to(&answer, asker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: Ipv4Addr = Ipv4Addr::new(192, 168, 71, 1);

    fn question(kind: u16) -> Vec<u8> {
        let mut question = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        question.extend_from_slice(b"\x07captive\x05apple\x03com\x00");
        question.extend_from_slice(&kind.to_be_bytes());
        question.extend_from_slice(&INTERNET.to_be_bytes());
        question
    }

    #[test]
    fn answers_every_name_with_its_address() {
        let question = question(ADDRESS);
        let answer = answer(&question, HERE).unwrap();

        assert_eq!(answer[..2], [0x12, 0x34]);
        assert_eq!(answer[2..4], [0x85, 0x80]);
        assert_eq!(answer[4..8], [0, 1, 0, 1]);
        assert_eq!(answer[12..question.len()], question[12..]);
        assert_eq!(answer[question.len()..question.len() + 2], [0xc0, 0x0c]);
        assert_eq!(answer[answer.len() - 4..], HERE.octets());
    }

    #[test]
    fn has_no_record_for_other_questions() {
        let question = question(28);
        let answer = answer(&question, HERE).unwrap();

        assert_eq!(answer[4..8], [0, 1, 0, 0]);
        assert_eq!(answer.len(), question.len());
    }

    #[test]
    fn ignores_what_is_not_one_question() {
        let whole = question(ADDRESS);
        for length in 0..whole.len() {
            assert_eq!(answer(&whole[..length], HERE), None);
        }
        let mut response = whole.clone();
        response[2] |= 0x80;
        assert_eq!(answer(&response, HERE), None);
        let mut two = whole.clone();
        two[5] = 2;
        assert_eq!(answer(&two, HERE), None);
        let mut pointed = whole;
        pointed[12] = 0xc0;
        assert_eq!(answer(&pointed, HERE), None);
    }
}
