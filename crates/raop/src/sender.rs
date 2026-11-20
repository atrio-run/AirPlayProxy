//! A session with a receiver over the sockets of the standard library.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::clock::{Clock, Ntp};
use crate::packet::{self, AUDIO_HEADER_BYTES, FRAMES_PER_PACKET, RETRANSMIT_HEADER};
use crate::rtsp::{self, Response};
use crate::{FRAME_BYTES, SAMPLE_RATE};

const USER_AGENT: &str = "AirPlay/550.10";
const PACKET_BYTES: usize = AUDIO_HEADER_BYTES + FRAMES_PER_PACKET * FRAME_BYTES;
const RTSP_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames a receiver holds audio beyond the latency it is asked for. A HomePod mini plays about
/// 285 ms later than asked, the 250 ms of these frames and the 43 ms it reports as its own.
const RECEIVER_HOLD: i32 = 11_025;
/// Packets kept for the receiver to ask again, beyond those it has not played yet.
const BACKLOG_MARGIN: usize = 16;
/// Stack of the threads that wait on the UDP sockets, where the default may be a few kilobytes.
const BACKGROUND_STACK: usize = 8192;
/// Frames between two signs of life on the RTSP connection. A HomePod mini ends a session
/// about half a minute after the last one.
const KEEP_ALIVE_FRAMES: u32 = 10 * SAMPLE_RATE;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The receiver answered a request with a status other than 200.
    Refused {
        request: &'static str,
        status: u16,
    },
    /// The receiver answered with something that is not an RTSP response, or left out a
    /// header the session needs.
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Refused { request, status } => {
                write!(formatter, "the receiver answered {status} to {request}")
            }
            Self::Malformed(what) => write!(formatter, "the receiver sent no valid {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn random() -> u64 {
    RandomState::new().build_hasher().finish()
}

struct Rtsp {
    stream: TcpStream,
    uri: String,
    identity: [(&'static str, String); 3],
    session: Option<String>,
    sequence: u32,
    received: Vec<u8>,
    /// Whether a read waits for data to arrive.
    blocking: bool,
}

impl Rtsp {
    /// Sends a request and returns the sequence number that its answer carries.
    fn request(
        &mut self,
        method: &str,
        target: Option<&str>,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> io::Result<String> {
        let sequence = self.sequence.to_string();
        self.sequence += 1;

        let mut all = vec![("CSeq", sequence.as_str()), ("User-Agent", USER_AGENT)];
        all.extend(
            self.identity
                .iter()
                .map(|(name, value)| (*name, value.as_str())),
        );
        if let Some(session) = &self.session {
            all.push(("Session", session));
        }
        all.extend_from_slice(headers);

        let target = target.unwrap_or(&self.uri);
        self.stream
            .write_all(&rtsp::request(method, target, &all, body))?;
        Ok(sequence)
    }

    /// Reads from the connection, as much as there is or, when blocking, at least something.
    fn read(&mut self) -> io::Result<()> {
        let mut chunk = [0; 1024];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(read) => self.received.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
            if !self.received.is_empty() && self.blocking {
                return Ok(());
            }
        }
    }

    /// Takes the first whole response out of what was read.
    fn response(&mut self) -> Result<Option<Response>, Error> {
        let parsed = Response::parse(&self.received).map_err(|_| Error::Malformed("response"))?;
        Ok(parsed.map(|(response, length)| {
            self.received.drain(..length);
            response
        }))
    }

    fn exchange(
        &mut self,
        method: &'static str,
        target: Option<&str>,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response, Error> {
        let sequence = self.request(method, target, headers, body)?;

        let response = loop {
            match self.response()? {
                Some(response) if response.header("CSeq") == Some(&sequence) => break response,
                // The answer to an earlier request that nothing waited for.
                Some(_) => {}
                None => self.read()?,
            }
        };

        if response.status != 200 {
            return Err(Error::Refused {
                request: method,
                status: response.status,
            });
        }
        Ok(response)
    }

    /// Sends a request whose answer nothing waits for, so that the audio goes on in time. The
    /// answers to the earlier ones are read here, whatever they say, which also tells when the
    /// receiver closed the connection.
    fn post(
        &mut self,
        method: &str,
        target: Option<&str>,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<(), Error> {
        self.stream.set_nonblocking(true)?;
        self.blocking = false;
        let read = self.read();
        self.stream.set_nonblocking(false)?;
        self.blocking = true;
        read?;
        while self.response()?.is_some() {}

        self.request(method, target, headers, body)?;
        Ok(())
    }

    /// Shows the receiver that the session is alive: it ends one it does not hear from.
    fn keep_alive(&mut self) -> Result<(), Error> {
        self.post("OPTIONS", Some("*"), &[], &[])
    }
}

/// The last packets sent, for the receiver to ask again. The packets share one allocation,
/// which is large enough for a system with external RAM to place it there.
struct Backlog {
    /// The sequence number and the length of the packet in each slot. An empty slot has none.
    slots: Vec<Option<(u16, usize)>>,
    packets: Vec<u8>,
}

impl Backlog {
    fn new(slots: usize) -> Self {
        Self {
            slots: vec![None; slots],
            packets: vec![0; slots * PACKET_BYTES],
        }
    }

    fn store(&mut self, sequence: u16, packet: &[u8]) {
        let slot = sequence as usize % self.slots.len();
        self.slots[slot] = Some((sequence, packet.len()));
        self.packets[slot * PACKET_BYTES..][..packet.len()].copy_from_slice(packet);
    }

    fn get(&self, sequence: u16) -> Option<&[u8]> {
        let slot = sequence as usize % self.slots.len();
        match self.slots[slot] {
            Some((stored, length)) if stored == sequence => {
                Some(&self.packets[slot * PACKET_BYTES..][..length])
            }
            _ => None,
        }
    }
}

/// How long the timing requests of the receiver took to arrive. The clocks of the two sides are
/// unrelated, so a request tells nothing alone: what tells is how the requests differ.
#[derive(Default)]
struct Transit {
    requests: u32,
    /// The smallest and the largest difference between the sender's clock when a request
    /// arrived and the receiver's clock when it left, in NTP units.
    range: Option<(i64, i64)>,
}

impl Transit {
    fn add(&mut self, sent: Ntp, arrived: Ntp) {
        let took = arrived.0.wrapping_sub(sent.0) as i64;
        self.requests += 1;
        self.range = Some(match self.range {
            Some((quickest, slowest)) => (quickest.min(took), slowest.max(took)),
            None => (took, took),
        });
    }

    /// How much longer the slowest request took than the quickest.
    fn spread(&self) -> Duration {
        let ntp = self.range.map_or(0, |(quickest, slowest)| (slowest - quickest) as u64);
        Duration::from_micros(((ntp as u128 * 1_000_000) >> 32) as u64)
    }
}

/// Stops the threads of a session, which wait on their sockets, when the session is dropped.
struct Background {
    stopping: Arc<AtomicBool>,
    threads: Vec<(SocketAddr, JoinHandle<()>)>,
}

impl Background {
    /// Runs `handle` on every packet that `socket` receives, with the address it came from.
    fn spawn(
        &mut self,
        socket: Arc<UdpSocket>,
        mut handle: impl FnMut(&[u8], SocketAddr) + Send + 'static,
    ) -> io::Result<()> {
        let address = socket.local_addr()?;
        let stopping = self.stopping.clone();
        let thread = thread::Builder::new()
            .stack_size(BACKGROUND_STACK)
            .spawn(move || {
                let mut packet = [0; 2048];
                while let Ok((length, sender)) = socket.recv_from(&mut packet) {
                    if stopping.load(Ordering::Relaxed) {
                        break;
                    }
                    handle(&packet[..length], sender);
                }
            })?;
        self.threads.push((address, thread));
        Ok(())
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        for (address, thread) in self.threads.drain(..) {
            // A packet to its own socket wakes the thread, which then sees it has to stop.
            let woken = UdpSocket::bind((address.ip(), 0))
                .and_then(|socket| socket.send_to(&[0], address))
                .is_ok();
            if woken {
                let _ = thread.join();
            }
        }
    }
}

pub struct Sender {
    rtsp: Rtsp,
    audio: UdpSocket,
    control: Arc<UdpSocket>,
    control_peer: SocketAddr,
    clock: Clock,
    backlog: Arc<Mutex<Backlog>>,
    retransmitted: Arc<AtomicU32>,
    transit: Arc<Mutex<Transit>>,
    latency: i32,
    /// Frames after which a packet sent late no longer plays.
    in_time: u64,
    receiver_latency: u32,
    source: u32,
    sequence: u16,
    /// The frame of the timeline that the next packet starts with.
    head: u64,
    starting: bool,
    frames_since_sync: u32,
    frames_since_keep_alive: u32,
    dropped: u32,
    fallen_behind: u32,
    /// The most frames a packet left after its time, since `longest_delay` was last asked.
    longest_delay: u64,
    packet: Vec<u8>,
    _background: Background,
}

impl Sender {
    /// Opens a session in which audio plays `latency` frames after it is sent, plus what the
    /// receiver holds it. A negative latency takes from that hold.
    pub fn connect(receiver: SocketAddr, latency: i32) -> Result<Self, Error> {
        let stream = TcpStream::connect_timeout(&receiver, RTSP_TIMEOUT)?;
        stream.set_read_timeout(Some(RTSP_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?.ip();

        let clock = Clock::new();
        let mut background = Background {
            stopping: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };

        // The receiver asks for the time before it answers SETUP.
        let timing = Arc::new(UdpSocket::bind((local, 0))?);
        let transit = Arc::new(Mutex::new(Transit::default()));
        let (replies, measured) = (timing.clone(), transit.clone());
        background.spawn(timing.clone(), move |request, receiver| {
            let now = clock.now();
            if let Some(reply) = packet::timing_reply(request, now) {
                let _ = replies.send_to(&reply, receiver);
            }
            if let Some(sent) = packet::timing_request(request) {
                measured.lock().unwrap().add(sent, now);
            }
        })?;

        let control = Arc::new(UdpSocket::bind((local, 0))?);
        let audio = UdpSocket::bind((local, 0))?;

        let mut rtsp = Rtsp {
            stream,
            uri: format!("rtsp://{local}/{}", random() as u32),
            identity: [
                ("DACP-ID", format!("{:016X}", random())),
                ("Active-Remote", (random() as u32).to_string()),
                ("Client-Instance", format!("{:016X}", random())),
            ],
            session: None,
            sequence: 0,
            received: Vec::new(),
            blocking: true,
        };

        rtsp.exchange(
            "ANNOUNCE",
            None,
            &[("Content-Type", "application/sdp")],
            session_description(local, receiver.ip(), &rtsp.uri).as_bytes(),
        )?;

        let transport = format!(
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={};timing_port={}",
            control.local_addr()?.port(),
            timing.local_addr()?.port(),
        );
        let setup = rtsp.exchange("SETUP", None, &[("Transport", &transport)], &[])?;
        let ports = setup.header("Transport").unwrap_or_default();
        let port = |name| rtsp::transport_port(ports, name).ok_or(Error::Malformed("Transport"));
        audio.connect((receiver.ip(), port("server_port")?))?;
        let control_peer = SocketAddr::new(receiver.ip(), port("control_port")?);
        rtsp.session = Some(
            setup
                .header("Session")
                .ok_or(Error::Malformed("Session"))?
                .to_owned(),
        );

        let sequence = random() as u16;
        let head = clock.now().frame();
        let record = rtsp.exchange(
            "RECORD",
            None,
            &[
                ("Range", "npt=0-"),
                (
                    "RTP-Info",
                    &format!("seq={sequence};rtptime={}", head as u32),
                ),
            ],
            &[],
        )?;
        let receiver_latency = record
            .header("Audio-Latency")
            .and_then(|latency| latency.parse().ok())
            .unwrap_or(0);

        let in_time = (latency + RECEIVER_HOLD).max(0) as u64;
        let slots = in_time as usize / FRAMES_PER_PACKET + BACKLOG_MARGIN;
        let backlog = Arc::new(Mutex::new(Backlog::new(slots)));
        let retransmitted = Arc::new(AtomicU32::new(0));
        let (sent, count, replies) = (backlog.clone(), retransmitted.clone(), control.clone());
        background.spawn(control.clone(), move |request, receiver| {
            let Some((first, packets)) = packet::retransmit_request(request) else {
                return;
            };
            let sent = sent.lock().unwrap();
            let mut reply = Vec::new();
            for sequence in (0..packets).map(|offset| first.wrapping_add(offset)) {
                if let Some(packet) = sent.get(sequence) {
                    reply.clear();
                    reply.extend_from_slice(&RETRANSMIT_HEADER);
                    reply.extend_from_slice(packet);
                    let _ = replies.send_to(&reply, receiver);
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }
        })?;

        Ok(Self {
            rtsp,
            audio,
            control,
            control_peer,
            clock,
            backlog,
            retransmitted,
            transit,
            latency,
            in_time,
            receiver_latency,
            source: random() as u32,
            sequence,
            head,
            starting: true,
            frames_since_sync: 0,
            frames_since_keep_alive: 0,
            dropped: 0,
            fallen_behind: 0,
            longest_delay: 0,
            packet: Vec::with_capacity(PACKET_BYTES),
            _background: background,
        })
    }

    /// The frames of latency the receiver reports for itself.
    pub fn receiver_latency(&self) -> u32 {
        self.receiver_latency
    }

    /// The packets that found the network buffers of the sender full and never left.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// The times the stream fell so far behind that it continued from the present, leaving a
    /// gap in what plays.
    pub fn fallen_behind(&self) -> u32 {
        self.fallen_behind
    }

    /// How long after its time the latest of the packets left, since this was last asked.
    pub fn longest_delay(&mut self) -> Duration {
        let frames = core::mem::take(&mut self.longest_delay);
        Duration::from_micros(frames * 1_000_000 / u64::from(SAMPLE_RATE))
    }

    /// The timing requests of the receiver that arrived since this was last asked, and how much
    /// longer the slowest of them took to arrive than the quickest. The receiver follows the
    /// sender's clock through the answers, and reads it the worse the more that differs.
    pub fn timing_requests(&mut self) -> (u32, Duration) {
        let transit = core::mem::take(&mut *self.transit.lock().unwrap());
        (transit.requests, transit.spread())
    }

    /// Counts a packet that found the network buffers full as lost on the way: the receiver
    /// asks for it again.
    fn sent(&mut self, result: io::Result<usize>) -> Result<(), Error> {
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::OutOfMemory => {
                self.dropped += 1;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The packets sent again at the receiver's request.
    pub fn retransmitted(&self) -> u32 {
        self.retransmitted.load(Ordering::Relaxed)
    }

    /// Sends one packet of `FRAMES_PER_PACKET` frames, at the time the timeline gives it: the
    /// call returns when the packet has left. A stream that starts, or that fell so far behind
    /// that its packets would no longer play, continues from the present.
    pub fn send(&mut self, frames: &[u8]) -> Result<(), Error> {
        assert_eq!(frames.len(), FRAMES_PER_PACKET * FRAME_BYTES);

        let now = self.clock.now().frame();
        if self.starting {
            self.head = now;
        } else if now > self.head + self.in_time {
            self.fallen_behind += 1;
            self.head = now;
            self.starting = true;
        } else if let Some(wait) = self.clock.until(Ntp::of_frame(self.head)) {
            thread::sleep(wait);
        } else {
            self.longest_delay = self.longest_delay.max(now.saturating_sub(self.head));
        }

        let timestamp = self.head as u32;
        if self.starting || self.frames_since_sync >= SAMPLE_RATE {
            let sync = packet::sync(
                self.starting,
                timestamp,
                self.latency,
                Ntp::of_frame(self.head),
            );
            let sent = self.control.send_to(&sync, self.control_peer);
            self.sent(sent)?;
            self.frames_since_sync = 0;
        }

        self.packet.clear();
        self.packet.extend_from_slice(&packet::audio_header(
            self.starting,
            self.sequence,
            timestamp,
            self.source,
        ));
        // The samples travel in network byte order.
        self.packet.extend(
            frames
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|sample| [sample[1], sample[0]]),
        );

        self.backlog
            .lock()
            .unwrap()
            .store(self.sequence, &self.packet);
        let sent = self.audio.send(&self.packet);
        self.sent(sent)?;

        if self.frames_since_keep_alive >= KEEP_ALIVE_FRAMES {
            self.rtsp.keep_alive()?;
            self.frames_since_keep_alive = 0;
        }

        self.sequence = self.sequence.wrapping_add(1);
        self.head += FRAMES_PER_PACKET as u64;
        self.frames_since_sync += FRAMES_PER_PACKET as u32;
        self.frames_since_keep_alive += FRAMES_PER_PACKET as u32;
        self.starting = false;
        Ok(())
    }

    /// Sets the volume of the receiver in decibel, from -30 to 0, or -144 to mute it. Nothing
    /// waits for the receiver's answer: a volume it refuses goes unnoticed.
    pub fn set_volume(&mut self, decibel: f32) -> Result<(), Error> {
        let parameter = format!("volume: {decibel:.6}\r\n");
        self.rtsp.post(
            "SET_PARAMETER",
            None,
            &[("Content-Type", "text/parameters")],
            parameter.as_bytes(),
        )
    }

    /// Makes the receiver drop the audio it has not played yet. The stream continues from the
    /// present with the next packet.
    pub fn flush(&mut self) -> Result<(), Error> {
        let position = format!("seq={};rtptime={}", self.sequence, self.head as u32);
        self.rtsp
            .exchange("FLUSH", None, &[("RTP-Info", &position)], &[])?;
        self.starting = true;
        Ok(())
    }

    pub fn close(mut self) -> Result<(), Error> {
        self.rtsp.exchange("TEARDOWN", None, &[], &[])?;
        Ok(())
    }
}

fn session_description(local: IpAddr, receiver: IpAddr, uri: &str) -> String {
    let session = uri.rsplit('/').next().unwrap_or_default();
    let family = |address: IpAddr| if address.is_ipv4() { "IP4" } else { "IP6" };
    format!(
        "v=0\r\n\
         o=iTunes {session} 0 IN {} {local}\r\n\
         s=iTunes\r\n\
         c=IN {} {receiver}\r\n\
         t=0 0\r\n\
         m=audio 0 RTP/AVP 96\r\n\
         a=rtpmap:96 L16/{SAMPLE_RATE}/2\r\n",
        family(local),
        family(receiver),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_session() {
        let description = session_description(
            "192.168.1.2".parse().unwrap(),
            "192.168.1.9".parse().unwrap(),
            "rtsp://192.168.1.2/77",
        );
        assert_eq!(
            description,
            "v=0\r\no=iTunes 77 0 IN IP4 192.168.1.2\r\ns=iTunes\r\nc=IN IP4 192.168.1.9\r\n\
             t=0 0\r\nm=audio 0 RTP/AVP 96\r\na=rtpmap:96 L16/44100/2\r\n"
        );
    }

    #[test]
    fn reads_past_the_answers_to_keep_alives() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut requests = Vec::new();
            for sequence in 0..3 {
                while requests.windows(4).filter(|end| end == b"\r\n\r\n").count() <= sequence {
                    let mut chunk = [0; 512];
                    let read = stream.read(&mut chunk).unwrap();
                    requests.extend_from_slice(&chunk[..read]);
                }
                let answer = format!("RTSP/1.0 200 OK\r\nCSeq: {sequence}\r\n\r\n");
                stream.write_all(answer.as_bytes()).unwrap();
            }
            String::from_utf8(requests).unwrap()
        });

        let mut rtsp = Rtsp {
            stream: TcpStream::connect(address).unwrap(),
            uri: "rtsp://127.0.0.1/1".to_owned(),
            identity: [
                ("DACP-ID", String::new()),
                ("Active-Remote", String::new()),
                ("Client-Instance", String::new()),
            ],
            session: None,
            sequence: 0,
            received: Vec::new(),
            blocking: true,
        };
        rtsp.keep_alive().unwrap();
        rtsp.keep_alive().unwrap();
        let answer = rtsp.exchange("TEARDOWN", None, &[], &[]).unwrap();

        assert_eq!(answer.header("CSeq"), Some("2"));
        let requests = receiver.join().unwrap();
        assert_eq!(requests.matches("OPTIONS * RTSP/1.0").count(), 2);
        assert!(requests.contains("TEARDOWN rtsp://127.0.0.1/1 RTSP/1.0"));
    }

    #[test]
    fn a_keep_alive_tells_when_the_receiver_closed_the_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        drop(listener.accept().unwrap());

        let mut rtsp = Rtsp {
            stream,
            uri: String::new(),
            identity: [
                ("DACP-ID", String::new()),
                ("Active-Remote", String::new()),
                ("Client-Instance", String::new()),
            ],
            session: None,
            sequence: 0,
            received: Vec::new(),
            blocking: true,
        };
        // The close may take a moment to cross the loopback.
        let closed = (0..50).any(|_| {
            thread::sleep(Duration::from_millis(10));
            rtsp.keep_alive().is_err()
        });
        assert!(closed);
    }

    #[test]
    fn tells_how_the_timing_requests_differ_in_arriving() {
        let second = 1i64 << 32;
        let mut transit = Transit::default();
        assert_eq!(transit.spread(), Duration::ZERO);

        // The receiver's clock is far ahead of the sender's: the differences are negative.
        let requests = [
            (1000 * second, second / 100),
            (1003 * second, second / 8),
            (1006 * second, second / 50),
        ];
        for (sent, took) in requests {
            let arrived = sent - 900 * second + took;
            transit.add(Ntp(sent as u64), Ntp(arrived as u64));
        }

        assert_eq!(transit.requests, 3);
        assert_eq!(transit.spread(), Duration::from_millis(115));
    }

    #[test]
    fn the_backlog_returns_only_the_packets_it_still_holds() {
        let mut backlog = Backlog::new(4);
        for sequence in 65_530..=65_535u16 {
            backlog.store(sequence, &sequence.to_be_bytes());
        }
        backlog.store(0, &[0, 0]);

        assert_eq!(backlog.get(65_531), None);
        assert_eq!(backlog.get(65_533), Some(&65_533u16.to_be_bytes()[..]));
        assert_eq!(backlog.get(0), Some(&[0, 0][..]));
        assert_eq!(backlog.get(1), None);
    }
}
