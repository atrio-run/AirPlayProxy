//! Sender side of RAOP, the audio streaming protocol of AirPlay 1.
//!
//! A session is an RTSP dialogue over TCP and three UDP flows: the audio packets, the control
//! packets that tie audio timestamps to the sender's clock and ask for lost packets again, and
//! the timing packets through which the receiver follows the sender's clock. The audio is
//! uncompressed and unencrypted: 16 bit, stereo, 44.1 kHz.
//!
//! The sender decides when audio plays. A frame plays `latency` frames after the moment it is
//! sent, plus what the receiver holds it on its own: about 285 ms on a HomePod mini.

mod clock;
mod packet;
mod rtsp;
mod sender;

pub use packet::FRAMES_PER_PACKET;
pub use sender::{Error, Sender};

pub const SAMPLE_RATE: u32 = 44_100;
/// Bytes of one frame handed to the sender: two little-endian 16-bit samples.
pub const FRAME_BYTES: usize = 4;
