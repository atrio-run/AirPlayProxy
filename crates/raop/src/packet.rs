//! The UDP packets of a session.

use crate::clock::Ntp;

pub const FRAMES_PER_PACKET: usize = 352;
pub const AUDIO_HEADER_BYTES: usize = 12;
/// Precedes an audio packet that is sent again on the control port.
pub const RETRANSMIT_HEADER: [u8; 4] = [0x80, 0xD6, 0x00, 0x01];

const TIMING_REQUEST: u8 = 0xD2;
const TIMING_REPLY: u8 = 0xD3;
const SYNC: u8 = 0xD4;
const RETRANSMIT_REQUEST: u8 = 0xD5;

/// The RTP header of an audio packet. `first` marks the packet that opens or resumes a stream.
pub fn audio_header(
    first: bool,
    sequence: u16,
    timestamp: u32,
    source: u32,
) -> [u8; AUDIO_HEADER_BYTES] {
    let mut header = [0; AUDIO_HEADER_BYTES];
    header[0] = 0x80;
    header[1] = if first { 0xE0 } else { 0x60 };
    header[2..4].copy_from_slice(&sequence.to_be_bytes());
    header[4..8].copy_from_slice(&timestamp.to_be_bytes());
    header[8..].copy_from_slice(&source.to_be_bytes());
    header
}

/// Ties the audio timeline to the sender's clock: the frame `timestamp` is sent at `time`, and
/// the frame `latency` before it plays at `time`. A negative latency names a frame still to be
/// sent.
pub fn sync(first: bool, timestamp: u32, latency: i32, time: Ntp) -> [u8; 20] {
    let mut packet = [0; 20];
    packet[0] = if first { 0x90 } else { 0x80 };
    packet[1] = SYNC;
    packet[3] = 7;
    packet[4..8].copy_from_slice(&timestamp.wrapping_sub(latency as u32).to_be_bytes());
    packet[8..16].copy_from_slice(&time.to_bytes());
    packet[16..].copy_from_slice(&timestamp.to_be_bytes());
    packet
}

/// The time, on the receiver's clock, at which it sent a timing request, or `None` if `request`
/// is not one.
pub fn timing_request(request: &[u8]) -> Option<Ntp> {
    if request.len() != 32 || request[1] != TIMING_REQUEST {
        return None;
    }
    let mut sent = [0; 8];
    sent.copy_from_slice(&request[24..]);
    Some(Ntp(u64::from_be_bytes(sent)))
}

/// The reply to a timing request of the receiver, or `None` if `request` is not one. The reply
/// returns the time the receiver sent the request with and gives the sender's time.
pub fn timing_reply(request: &[u8], now: Ntp) -> Option<[u8; 32]> {
    timing_request(request)?;

    let mut reply = [0; 32];
    reply[0] = 0x80;
    reply[1] = TIMING_REPLY;
    reply[3] = 7;
    reply[8..16].copy_from_slice(&request[24..]);
    reply[16..24].copy_from_slice(&now.to_bytes());
    reply[24..].copy_from_slice(&now.to_bytes());
    Some(reply)
}

/// The audio packets the receiver asks for again: the first sequence number and how many.
pub fn retransmit_request(packet: &[u8]) -> Option<(u16, u16)> {
    if packet.len() < 8 || packet[1] != RETRANSMIT_REQUEST {
        return None;
    }
    Some((
        u16::from_be_bytes([packet[4], packet[5]]),
        u16::from_be_bytes([packet[6], packet[7]]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_header_marks_the_first_packet() {
        assert_eq!(
            audio_header(true, 0x0102, 0x0A0B0C0D, 0x11223344),
            [0x80, 0xE0, 0x01, 0x02, 0x0A, 0x0B, 0x0C, 0x0D, 0x11, 0x22, 0x33, 0x44]
        );
        assert_eq!(audio_header(false, 0, 0, 0)[1], 0x60);
    }

    #[test]
    fn sync_carries_both_timestamps_and_the_time() {
        let packet = sync(true, 100_000, 11_025, Ntp(0x0102030405060708));
        assert_eq!(packet[..4], [0x90, 0xD4, 0x00, 0x07]);
        assert_eq!(packet[4..8], 88_975u32.to_be_bytes());
        assert_eq!(packet[8..16], [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(packet[16..], 100_000u32.to_be_bytes());
        assert_eq!(sync(false, 0, 0, Ntp(0))[0], 0x80);
    }

    #[test]
    fn sync_wraps_with_the_timestamp() {
        let packet = sync(false, 5, 10, Ntp(0));
        assert_eq!(packet[4..8], (u32::MAX - 4).to_be_bytes());
    }

    #[test]
    fn sync_takes_a_negative_latency() {
        let packet = sync(false, 100_000, -4_410, Ntp(0));
        assert_eq!(packet[4..8], 104_410u32.to_be_bytes());
    }

    #[test]
    fn timing_reply_returns_the_time_of_the_request() {
        // A request of a HomePod mini, which had not followed any clock yet.
        let request = [
            0x80, 0xD2, 0x00, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x83, 0xAA, 0x7E, 0x80, 0xE7, 0x47, 0xE8, 0xCC,
        ];
        let reply = timing_reply(&request, Ntp(0xEEA86EC82A0EE1C6)).unwrap();

        assert_eq!(reply[..8], [0x80, 0xD3, 0x00, 0x07, 0, 0, 0, 0]);
        assert_eq!(reply[8..16], request[24..]);
        assert_eq!(reply[16..24], 0xEEA86EC82A0EE1C6u64.to_be_bytes());
        assert_eq!(reply[24..], 0xEEA86EC82A0EE1C6u64.to_be_bytes());
    }

    #[test]
    fn timing_reply_ignores_other_packets() {
        assert!(timing_reply(&[0; 32], Ntp(0)).is_none());
        assert!(timing_reply(&[0x80, 0xD2, 0x00], Ntp(0)).is_none());
    }

    #[test]
    fn reads_when_a_timing_request_was_sent() {
        let mut request = [0; 32];
        request[..4].copy_from_slice(&[0x80, 0xD2, 0x00, 0x07]);
        request[24..].copy_from_slice(&[0x83, 0xAA, 0x7E, 0x80, 0xE7, 0x47, 0xE8, 0xCC]);

        assert_eq!(timing_request(&request), Some(Ntp(0x83AA7E80E747E8CC)));
        assert_eq!(timing_request(&[0; 32]), None);
    }

    #[test]
    fn reads_a_retransmit_request() {
        let request = [0x80, 0xD5, 0x00, 0x01, 0x12, 0x34, 0x00, 0x03];
        assert_eq!(retransmit_request(&request), Some((0x1234, 3)));
        assert_eq!(retransmit_request(&request[..7]), None);
        assert_eq!(retransmit_request(&[0x80, 0xD4, 0, 0, 0, 0, 0, 0]), None);
    }
}
