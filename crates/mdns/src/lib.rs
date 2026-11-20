//! Multicast DNS, as far as a small device needs it: the IPv4 address of a host of the local
//! network by its `.local` name, the hosts that offer a service, and the answer to who asks for
//! the device's own name.
//!
//! A question goes to every host on the network, over IPv6 and over IPv4, and the hosts that
//! have an answer send it to the socket that asked. Both ways are asked because a host may answer
//! on one and not on the other: a HomePod mini was seen to answer over IPv6 only.

mod message;

use std::convert::Infallible;
use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub use message::Service;

const PORT: u16 = 5353;
const EVERY_HOST_IPV4: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const EVERY_HOST_IPV6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
/// How long answers are awaited on each way before the question is asked again.
const TURN: Duration = Duration::from_millis(250);
const LONGEST_MESSAGE: usize = 1500;

/// Asks `question` of every host, again and again for `patience`, and hands each message that
/// comes back to `heard`, until it returns something. `interface` is the index of the network
/// interface that the question leaves from over IPv6.
fn ask<T>(
    question: &[u8],
    interface: u32,
    patience: Duration,
    mut heard: impl FnMut(&[u8]) -> Option<T>,
) -> io::Result<Option<T>> {
    let ways = [
        (
            UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))?,
            SocketAddr::V6(SocketAddrV6::new(EVERY_HOST_IPV6, PORT, 0, interface)),
        ),
        (
            UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?,
            SocketAddr::V4(SocketAddrV4::new(EVERY_HOST_IPV4, PORT)),
        ),
    ];

    let give_up = Instant::now() + patience;
    let mut message = vec![0; LONGEST_MESSAGE];
    let mut asked = false;
    let mut refused = None;
    loop {
        for (socket, every_host) in &ways {
            match socket.send_to(question, every_host) {
                Ok(_) => asked = true,
                Err(error) => refused = Some(error),
            }
        }
        for (socket, _) in &ways {
            if let Some(found) = listen(socket, &mut message, &mut heard)? {
                return Ok(Some(found));
            }
        }
        if Instant::now() >= give_up {
            return match refused {
                Some(error) if !asked => Err(error),
                _ => Ok(None),
            };
        }
    }
}

/// Hands the messages that reach `socket` within a turn to `heard`, until it returns something.
fn listen<T>(
    socket: &UdpSocket,
    message: &mut [u8],
    heard: &mut impl FnMut(&[u8]) -> Option<T>,
) -> io::Result<Option<T>> {
    let until = Instant::now() + TURN;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        socket.set_read_timeout(Some(left))?;
        match socket.recv_from(message) {
            Ok((length, _)) => {
                if let Some(found) = heard(&message[..length]) {
                    return Ok(Some(found));
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    }
}

/// The IPv4 address of `host`, asked for `patience` at most. `interface` is the index of the
/// network interface that the question leaves from over IPv6.
pub fn resolve(host: &str, interface: u32, patience: Duration) -> io::Result<Ipv4Addr> {
    let question = message::query(host)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, format!("{host} is not a name")))?;
    ask(&question, interface, patience, |answer| {
        message::address(answer, host)
    })?
    .ok_or_else(|| io::Error::new(ErrorKind::TimedOut, format!("no host answered for {host}")))
}

/// The hosts that answer, within `patience`, that they offer `service`, such as
/// `_raop._tcp.local`, sorted by name. `interface` is the index of the network interface that
/// the question leaves from over IPv6.
pub fn browse(service: &str, interface: u32, patience: Duration) -> io::Result<Vec<Service>> {
    let question = message::services_query(service).ok_or_else(|| {
        io::Error::new(ErrorKind::InvalidInput, format!("{service} is not a name"))
    })?;
    let mut found: Vec<Service> = Vec::new();
    ask(&question, interface, patience, |answer| {
        for service in message::services(answer, service) {
            match found.iter_mut().find(|known| known.name == service.name) {
                Some(known) if known.address.is_none() => *known = service,
                Some(_) => {}
                None => found.push(service),
            }
        }
        None::<()>
    })?;
    found.sort_by(|one, other| one.name.cmp(&other.name));
    Ok(found)
}

/// The host that offers `service` under `name`, asked for `patience` at most.
pub fn find(service: &str, name: &str, interface: u32, patience: Duration) -> io::Result<Service> {
    let question = message::services_query(service).ok_or_else(|| {
        io::Error::new(ErrorKind::InvalidInput, format!("{service} is not a name"))
    })?;
    ask(&question, interface, patience, |answer| {
        message::services(answer, service)
            .into_iter()
            .find(|found| found.name == name)
    })?
    .ok_or_else(|| io::Error::new(ErrorKind::TimedOut, format!("no host answered as {name}")))
}

/// The name, without `.local`, under which a device called `name` answers: its letters and
/// digits in lower case, with one hyphen for whatever stands between them.
pub fn host_name(name: &str) -> String {
    let mut host = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            host.push(character.to_ascii_lowercase());
        } else if !host.is_empty() && !host.ends_with('-') {
            host.push('-');
        }
    }
    host.trim_end_matches('-').to_owned()
}

/// Makes what `socket` sends to every host leave from the network interface that has `address`.
/// A device with several interfaces otherwise sends it from one of its choosing, which may not
/// be the one the question came from.
fn send_from(socket: &UdpSocket, address: Ipv4Addr) -> io::Result<()> {
    let interface = libc::in_addr {
        s_addr: u32::from_ne_bytes(address.octets()),
    };
    let set = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            (&raw const interface).cast(),
            size_of::<libc::in_addr>() as libc::socklen_t,
        )
    };
    if set == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Answers, for as long as the device runs, the hosts that ask for an address of `host`: it has
/// this IPv4 address and no other.
/// `address` gives the address at that moment, or `None` while the device has none.
pub fn answer_for(host: &str, address: impl Fn() -> Option<Ipv4Addr>) -> io::Result<Infallible> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT))?;
    socket.join_multicast_v4(&EVERY_HOST_IPV4, &Ipv4Addr::UNSPECIFIED)?;
    // Multicast DNS hosts send every message with this hop limit, and may ignore one without.
    socket.set_multicast_ttl_v4(255)?;
    socket.set_ttl(255)?;

    let mut question = vec![0; LONGEST_MESSAGE];
    loop {
        let (length, asker) = socket.recv_from(&mut question)?;
        if message::asks_for(&question[..length], host).is_none() {
            continue;
        }
        let Some(address) = address() else {
            continue;
        };
        // An asker on another port is not a multicast DNS host: it takes an answer sent to it
        // alone, with the number of its question. A multicast DNS host is answered through every
        // host, as it expects, and alone as well: a network may not carry to it what is sent to
        // every host, and a Mac takes the answer either way.
        let multicast_host = asker.port() == PORT;
        let id = if multicast_host {
            [0, 0]
        } else {
            [question[0], question[1]]
        };
        let Some(answer) = message::answer(host, address, id) else {
            continue;
        };
        if multicast_host {
            let every_host = SocketAddr::from((EVERY_HOST_IPV4, PORT));
            let sent = send_from(&socket, address).and_then(|()| socket.send_to(&answer, every_host));
            if let Err(error) = sent {
                log::warn!("The answer to every host, that {host} is here, did not leave: {error}");
            }
        }
        if let Err(error) = socket.send_to(&answer, asker) {
            log::warn!("The answer to {asker}, who asked for {host}, did not leave: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_host_after_the_device() {
        assert_eq!(host_name("AirPlay Proxy"), "airplay-proxy");
        assert_eq!(host_name("  Sala cinema (1°)  "), "sala-cinema-1");
        assert_eq!(host_name("Caffè"), "caff");
        assert_eq!(host_name("!!!"), "");
    }
}
