//! The DNS messages of the questions for an address and for the hosts that offer a service, and
//! of their answers.

use std::net::Ipv4Addr;

const HEADER_BYTES: usize = 12;
const RESPONSE: u8 = 0x80;
/// The flags of an answer from the host that owns the name.
const AUTHORITATIVE_ANSWER: [u8; 2] = [0x84, 0x00];
const POINTER: u8 = 0xC0;
const LONGEST_LABEL: usize = 63;
const LONGEST_NAME: usize = 255;
const ADDRESS: u16 = 1;
const POINTER_TO_NAME: u16 = 12;
const IPV6_ADDRESS: u16 = 28;
const SERVICE: u16 = 33;
/// The record that tells which records a name has, and so which it has not.
const ONLY: u16 = 47;
const ANY: u16 = 255;
const INTERNET: u16 = 1;
/// In the class of an answer, tells the asker to forget what it held for the name. In the class
/// of a question, tells that the asker would rather have the answer alone.
const TOP_BIT: u16 = 0x8000;
/// Seconds an asker may hold the address this host answers with.
const ADDRESS_LIFETIME: u32 = 120;

/// A host that offers a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// The name the host gives to what it offers, such as `0123456789AB@Kitchen`.
    pub name: String,
    pub host: String,
    pub port: u16,
    /// The address of the host, when the answer came with it.
    pub address: Option<Ipv4Addr>,
}

/// A question of `kind` about `name`, or `None` if `name` is not a name.
fn question(name: &str, kind: u16) -> Option<Vec<u8>> {
    let mut message = vec![0; HEADER_BYTES];
    message[5] = 1;
    labels(&mut message, name)?;
    message.extend_from_slice(&kind.to_be_bytes());
    message.extend_from_slice(&INTERNET.to_be_bytes());
    Some(message)
}

/// Appends `name` as labels, or returns `None` if it is not a name.
fn labels(message: &mut Vec<u8>, name: &str) -> Option<()> {
    let start = message.len();
    for label in name.strip_suffix('.').unwrap_or(name).split('.') {
        if label.is_empty() || label.len() > LONGEST_LABEL {
            return None;
        }
        message.push(label.len() as u8);
        message.extend_from_slice(label.as_bytes());
    }
    message.push(0);
    (message.len() - start <= LONGEST_NAME).then_some(())
}

/// The question for the IPv4 address of `host`, or `None` if `host` is not a name.
pub fn query(host: &str) -> Option<Vec<u8>> {
    question(host, ADDRESS)
}

/// The question for the hosts that offer `service`, such as `_raop._tcp.local`.
pub fn services_query(service: &str) -> Option<Vec<u8>> {
    question(service, POINTER_TO_NAME)
}

struct Record<'a> {
    owner: String,
    kind: u16,
    class: u16,
    data: &'a [u8],
    /// Where `data` starts in the message: a name in it may point elsewhere in the message.
    at: usize,
}

/// The records of an answer, of all its sections, or `None` if `message` is not a whole answer.
fn records(message: &[u8]) -> Option<Vec<Record<'_>>> {
    if message.get(2)? & RESPONSE == 0 {
        return None;
    }
    let questions = field(message, 4)?;
    let count = field(message, 6)? as usize + field(message, 8)? as usize + field(message, 10)? as usize;

    let mut at = HEADER_BYTES;
    for _ in 0..questions {
        at = name(message, at)?.1 + 4;
    }
    let mut records = Vec::new();
    for _ in 0..count {
        let (owner, after) = name(message, at)?;
        let kind = field(message, after)?;
        let class = field(message, after + 2)? & !TOP_BIT;
        let length = field(message, after + 8)? as usize;
        let data = message.get(after + 10..after + 10 + length)?;
        records.push(Record {
            owner,
            kind,
            class,
            data,
            at: after + 10,
        });
        at = after + 10 + length;
    }
    Some(records)
}

fn bare(name: &str) -> &str {
    name.strip_suffix('.').unwrap_or(name)
}

fn same(name: &str, other: &str) -> bool {
    bare(name).eq_ignore_ascii_case(bare(other))
}

fn address_of(records: &[Record], host: &str) -> Option<Ipv4Addr> {
    records
        .iter()
        .filter(|record| record.kind == ADDRESS && record.class == INTERNET)
        .filter(|record| same(&record.owner, host))
        .find_map(|record| <[u8; 4]>::try_from(record.data).ok())
        .map(Ipv4Addr::from)
}

/// The IPv4 address that `message` gives for `host`, if it is an answer that gives one.
pub fn address(message: &[u8], host: &str) -> Option<Ipv4Addr> {
    address_of(&records(message)?, host)
}

/// The hosts that `message` tells offer `service`, if it is an answer that tells any.
pub fn services(message: &[u8], service: &str) -> Vec<Service> {
    let Some(records) = records(message) else {
        return Vec::new();
    };
    let suffix_length = bare(service).len() + 1;

    records
        .iter()
        .filter(|record| record.kind == SERVICE && record.class == INTERNET)
        .filter_map(|record| {
            let split = record.owner.len().checked_sub(suffix_length)?;
            if !same(record.owner.get(split + 1..)?, service) {
                return None;
            }
            let port = field(record.data, 4)?;
            let (host, _) = name(message, record.at + 6)?;
            Some(Service {
                name: record.owner.get(..split)?.to_owned(),
                address: address_of(&records, &host),
                host,
                port,
            })
        })
        .collect()
}

/// Whether `message` is a question for an address of `host`, of either kind.
pub fn asks_for(message: &[u8], host: &str) -> Option<()> {
    if message.get(2)? & RESPONSE != 0 {
        return None;
    }
    let mut at = HEADER_BYTES;
    for _ in 0..field(message, 4)? {
        let (asked, after) = name(message, at)?;
        let kind = field(message, after)?;
        let class = field(message, after + 2)?;
        at = after + 4;
        let about_addresses = matches!(kind, ADDRESS | IPV6_ADDRESS | ANY);
        if about_addresses && class & !TOP_BIT == INTERNET && same(&asked, host) {
            return Some(());
        }
    }
    None
}

/// The answer that `host` has `address` and no other record, or `None` if `host` is not a name.
/// An asker that wants both an IPv4 and an IPv6 address, as a Mac does, otherwise waits for the
/// second until it gives up on the name. `id` is the number of the question, which an asker that
/// is not a multicast DNS host expects back.
pub fn answer(host: &str, address: Ipv4Addr, id: [u8; 2]) -> Option<Vec<u8>> {
    let mut message = vec![0; HEADER_BYTES];
    message[..2].copy_from_slice(&id);
    message[2..4].copy_from_slice(&AUTHORITATIVE_ANSWER);
    message[7] = 2;

    let mut owner = Vec::new();
    labels(&mut owner, host)?;
    let record = |message: &mut Vec<u8>, kind: u16, data: &[u8]| {
        message.extend_from_slice(&owner);
        message.extend_from_slice(&kind.to_be_bytes());
        message.extend_from_slice(&(INTERNET | TOP_BIT).to_be_bytes());
        message.extend_from_slice(&ADDRESS_LIFETIME.to_be_bytes());
        message.extend_from_slice(&(data.len() as u16).to_be_bytes());
        message.extend_from_slice(data);
    };
    record(&mut message, ADDRESS, &address.octets());
    // The name itself, then the kinds of record it has, one bit each: the IPv4 address is the
    // second bit of the first byte.
    let mut only = owner.clone();
    only.extend_from_slice(&[0, 1, 0x40]);
    record(&mut message, ONLY, &only);
    Some(message)
}

fn field(message: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(message.get(at..at + 2)?.try_into().ok()?))
}

/// The name that starts at `at`, and where the message goes on after it. A name may end with a
/// pointer to the rest of it, earlier in the message; one whose pointers lead back into itself
/// grows past the longest name and is refused.
fn name(message: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut after = None;
    loop {
        let length = *message.get(at)?;
        if length & POINTER == POINTER {
            let target = (usize::from(length & !POINTER) << 8) | usize::from(*message.get(at + 1)?);
            after.get_or_insert(at + 2);
            if target >= at {
                return None;
            }
            at = target;
        } else if length == 0 {
            return Some((name, after.unwrap_or(at + 1)));
        } else {
            let label = message.get(at + 1..at + 1 + usize::from(length))?;
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(core::str::from_utf8(label).ok()?);
            if name.len() > LONGEST_NAME {
                return None;
            }
            at += 1 + usize::from(length);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a HomePod mini answers to the question for its address: the question, its IPv4
    /// address and its two IPv6 addresses.
    const ANSWER: &[u8] = &[
        0x00, 0x00, 0x84, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x08, 0x48, 0x6f,
        0x6d, 0x65, 0x50, 0x6f, 0x64, 0x31, 0x05, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x00, 0x00, 0x01,
        0x00, 0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x04, 0xc0,
        0xa8, 0x01, 0x14, 0xc0, 0x0c, 0x00, 0x1c, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x10,
        0xfe, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01, 0xc0, 0x0c, 0x00, 0x1c, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x10, 0x20, 0x01,
        0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    ];

    #[test]
    fn asks_for_the_address_of_a_name() {
        let expected = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x48,
            0x6f, 0x6d, 0x65, 0x50, 0x6f, 0x64, 0x31, 0x05, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x00,
            0x00, 0x01, 0x00, 0x01,
        ];
        assert_eq!(query("HomePod1.local").unwrap(), expected);
        assert_eq!(query("HomePod1.local.").unwrap(), expected);
    }

    #[test]
    fn refuses_what_is_not_a_name() {
        assert_eq!(query(""), None);
        assert_eq!(query("HomePod1..local"), None);
        assert_eq!(query(&format!("{}.local", "a".repeat(64))), None);
        assert_eq!(query(&["abcdefgh"; 32].join(".")), None);
    }

    #[test]
    fn reads_the_address_a_homepod_answered() {
        assert_eq!(
            address(ANSWER, "HomePod1.local"),
            Some(Ipv4Addr::new(192, 168, 1, 20))
        );
        assert_eq!(
            address(ANSWER, "homepod1.LOCAL."),
            Some(Ipv4Addr::new(192, 168, 1, 20))
        );
    }

    #[test]
    fn reads_an_address_marked_to_replace_what_was_held() {
        let mut answer = ANSWER.to_vec();
        answer[36] = 0x80;
        assert_eq!(
            address(&answer, "HomePod1.local"),
            Some(Ipv4Addr::new(192, 168, 1, 20))
        );
    }

    #[test]
    fn ignores_the_answer_for_another_name() {
        assert_eq!(address(ANSWER, "Tv.local"), None);
    }

    #[test]
    fn ignores_a_question() {
        assert_eq!(
            address(&query("HomePod1.local").unwrap(), "HomePod1.local"),
            None
        );
    }

    #[test]
    fn ignores_an_answer_without_an_ipv4_address() {
        let mut answer = ANSWER.to_vec();
        answer[35] = 0x1c;
        assert_eq!(address(&answer, "HomePod1.local"), None);
    }

    #[test]
    fn ignores_what_is_cut_short() {
        for length in 0..ANSWER.len().min(48) {
            assert_eq!(address(&ANSWER[..length], "HomePod1.local"), None);
        }
    }

    #[test]
    fn ignores_a_name_that_points_at_itself() {
        let mut answer = ANSWER.to_vec();
        answer[33] = 32;
        assert_eq!(address(&answer, "HomePod1.local"), None);
    }

    #[test]
    fn ignores_a_name_that_leads_back_into_itself() {
        let answer = [
            0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, b'a',
            0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x04, 192, 168, 1,
            20,
        ];
        assert_eq!(address(&answer, "a"), None);
    }

    /// An answer in the shape of a HomePod's to the question for `_raop._tcp.local`: the
    /// pointer to its service, then the service, its text and the address of its host.
    fn raop_answer() -> Vec<u8> {
        let mut message = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        // 12: _raop._tcp.local
        message.extend_from_slice(b"\x05_raop\x04_tcp\x05local\x00");
        message.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0x11, 0x94]);
        let instance = b"\x15ABCDEF012345@HomePod1\xc0\x0c";
        message.extend_from_slice(&(instance.len() as u16).to_be_bytes());
        let instance_at = message.len() as u8;
        message.extend_from_slice(instance);
        // The service: priority, weight, port 7000, host HomePod1.local.
        message.extend_from_slice(&[0xc0, instance_at, 0, 33, 0x80, 1, 0, 0, 0, 120]);
        let host = b"\x08HomePod1\xc0\x17";
        message.extend_from_slice(&((6 + host.len()) as u16).to_be_bytes());
        message.extend_from_slice(&[0, 0, 0, 0, 0x1b, 0x58]);
        let host_at = message.len() as u8;
        message.extend_from_slice(host);
        message.extend_from_slice(&[0xc0, instance_at, 0, 16, 0x80, 1, 0, 0, 0x11, 0x94, 0, 5]);
        message.extend_from_slice(b"\x04et=0");
        message.extend_from_slice(&[0xc0, host_at, 0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4]);
        message.extend_from_slice(&[192, 168, 1, 20]);
        message
    }

    #[test]
    fn asks_for_the_hosts_that_offer_a_service() {
        let query = services_query("_raop._tcp.local").unwrap();
        assert_eq!(query[..12], [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(query[12..30], *b"\x05_raop\x04_tcp\x05local\x00");
        assert_eq!(query[30..], [0, 12, 0, 1]);
    }

    #[test]
    fn reads_the_hosts_that_offer_a_service() {
        let expected = Service {
            name: "ABCDEF012345@HomePod1".to_owned(),
            host: "HomePod1.local".to_owned(),
            port: 7000,
            address: Some(Ipv4Addr::new(192, 168, 1, 20)),
        };
        assert_eq!(services(&raop_answer(), "_raop._tcp.local"), std::slice::from_ref(&expected));
        assert_eq!(services(&raop_answer(), "_RAOP._tcp.local."), [expected]);
    }

    #[test]
    fn reads_a_host_whose_address_did_not_come_along() {
        let mut answer = raop_answer();
        answer[11] = 2;
        let found = services(&answer, "_raop._tcp.local");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].address, None);
        assert_eq!(found[0].port, 7000);
    }

    #[test]
    fn ignores_the_hosts_of_another_service() {
        assert!(services(&raop_answer(), "_airplay._tcp.local").is_empty());
        assert!(services(ANSWER, "_raop._tcp.local").is_empty());
        let answer = raop_answer();
        for length in 0..answer.len() {
            assert!(services(&answer[..length], "_raop._tcp.local").is_empty());
        }
    }

    #[test]
    fn tells_a_question_for_its_own_name() {
        let mut question = query("airplay-proxy.local").unwrap();
        assert!(asks_for(&question, "AirPlay-Proxy.local").is_some());
        assert!(asks_for(&question, "other.local").is_none());

        // A Mac also asks for the IPv6 address.
        let kind = question.len() - 3;
        question[kind] = 28;
        assert!(asks_for(&question, "airplay-proxy.local").is_some());
        question[kind] = 1;

        // The asker would rather have the answer alone.
        let class = question.len() - 2;
        question[class] = 0x80;
        assert!(asks_for(&question, "airplay-proxy.local").is_some());

        assert!(asks_for(ANSWER, "HomePod1.local").is_none());
        let services = services_query("airplay-proxy.local").unwrap();
        assert!(asks_for(&services, "airplay-proxy.local").is_none());
    }

    #[test]
    fn answers_with_its_address() {
        let address = Ipv4Addr::new(192, 168, 1, 30);
        let answer = answer("airplay-proxy.local", address, [0x12, 0x34]).unwrap();

        assert_eq!(answer[..2], [0x12, 0x34]);
        assert_eq!(super::address(&answer, "airplay-proxy.local"), Some(address));

        // What a Mac took for the name on 20 November 2026: the address, and that there is no
        // other record.
        let name = b"\x0dairplay-proxy\x05local\x00";
        let mut expected = vec![0x12, 0x34, 0x84, 0, 0, 0, 0, 2, 0, 0, 0, 0];
        expected.extend_from_slice(name);
        expected.extend_from_slice(&[0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4, 192, 168, 1, 30]);
        expected.extend_from_slice(name);
        expected.extend_from_slice(&[0, 47, 0x80, 1, 0, 0, 0, 120, 0, 24]);
        expected.extend_from_slice(name);
        expected.extend_from_slice(&[0, 1, 0x40]);
        assert_eq!(answer, expected);
        assert_eq!(super::answer("not..a.name", address, [0, 0]), None);
    }
}
