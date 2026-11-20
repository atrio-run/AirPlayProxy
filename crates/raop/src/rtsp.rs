//! RTSP requests and responses, as an AirPlay receiver exchanges them.

#[derive(Debug, PartialEq, Eq)]
pub struct Malformed;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
}

impl Response {
    /// Reads a response from the beginning of `bytes`: the response and its length, body
    /// included, or `None` while part of it is still to arrive.
    pub fn parse(bytes: &[u8]) -> Result<Option<(Self, usize)>, Malformed> {
        let Some(head_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            return Ok(None);
        };
        let head = std::str::from_utf8(&bytes[..head_end]).map_err(|_| Malformed)?;
        let mut lines = head.split("\r\n");

        let status = lines
            .next()
            .and_then(|line| line.split(' ').nth(1))
            .and_then(|status| status.parse().ok())
            .ok_or(Malformed)?;
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(':').ok_or(Malformed)?;
                Ok((name.trim().to_owned(), value.trim().to_owned()))
            })
            .collect::<Result<_, _>>()?;
        let response = Self { status, headers };

        let body = match response.header("Content-Length") {
            Some(length) => length.parse::<usize>().map_err(|_| Malformed)?,
            None => 0,
        };
        let length = head_end + 4 + body;
        Ok((bytes.len() >= length).then_some((response, length)))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub fn request(method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut request = format!("{method} {target} RTSP/1.0\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");

    let mut request = request.into_bytes();
    request.extend_from_slice(body);
    request
}

/// A port of the `Transport` header that answers SETUP.
pub fn transport_port(transport: &str, name: &str) -> Option<u16> {
    transport
        .split(';')
        .filter_map(|option| option.split_once('='))
        .find(|(option, _)| *option == name)
        .and_then(|(_, port)| port.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a HomePod mini answers to SETUP.
    const SETUP: &[u8] = b"RTSP/1.0 200 OK\r\n\
        Date: Wed, 18 Nov 2026 18:36:56 GMT\r\n\
        Content-Length: 0\r\n\
        Transport: RTP/AVP/UDP;unicast;mode=record;server_port=61320;control_port=56736;timing_port=0\r\n\
        Session: 1\r\n\
        Audio-Jack-Status: connected\r\n\
        Server: AirTunes/870.14.1\r\n\
        CSeq: 1\r\n\r\n";

    #[test]
    fn reads_status_and_headers() {
        let (response, length) = Response::parse(SETUP).unwrap().unwrap();

        assert_eq!(length, SETUP.len());
        assert_eq!(response.status, 200);
        assert_eq!(response.header("Session"), Some("1"));
        assert_eq!(response.header("cseq"), Some("1"));
        assert_eq!(response.header("Missing"), None);
    }

    #[test]
    fn reads_the_ports_of_the_transport() {
        let (response, _) = Response::parse(SETUP).unwrap().unwrap();
        let transport = response.header("Transport").unwrap();

        assert_eq!(transport_port(transport, "server_port"), Some(61320));
        assert_eq!(transport_port(transport, "control_port"), Some(56736));
        assert_eq!(transport_port(transport, "timing_port"), Some(0));
        assert_eq!(transport_port(transport, "event_port"), None);
    }

    #[test]
    fn waits_for_the_whole_response() {
        for length in 0..SETUP.len() {
            assert!(Response::parse(&SETUP[..length]).unwrap().is_none());
        }
    }

    #[test]
    fn counts_the_body_in_the_length() {
        let response = b"RTSP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nhelloRTSP/1.0";

        assert!(Response::parse(&response[..40]).unwrap().is_none());
        let (_, length) = Response::parse(response).unwrap().unwrap();
        assert_eq!(&response[length - 5..length], b"hello");
    }

    #[test]
    fn reports_other_statuses() {
        let (response, _) = Response::parse(b"RTSP/1.0 453 Not Enough Bandwidth\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(response.status, 453);
    }

    #[test]
    fn rejects_what_is_not_a_response() {
        assert_eq!(Response::parse(b"hello\r\n\r\n").unwrap_err(), Malformed);
        assert_eq!(
            Response::parse(b"RTSP/1.0 200 OK\r\nno colon\r\n\r\n").unwrap_err(),
            Malformed
        );
    }

    #[test]
    fn writes_a_request() {
        let request = request(
            "ANNOUNCE",
            "rtsp://10.0.0.2/1",
            &[("CSeq", "0"), ("Content-Type", "application/sdp")],
            b"v=0\r\n",
        );
        assert_eq!(
            request,
            b"ANNOUNCE rtsp://10.0.0.2/1 RTSP/1.0\r\nCSeq: 0\r\nContent-Type: application/sdp\r\n\
              Content-Length: 5\r\n\r\nv=0\r\n"
        );
    }
}
