//! A web server for the few pages of a small device.
//!
//! It serves one request at a time, each on a connection of its own, to a handler that turns a
//! request into a response. It reads the forms that a browser posts and nothing else: no
//! uploads, no connections kept open.

use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

/// A request with a larger head or a larger body is refused.
const LONGEST_HEAD: usize = 8 * 1024;
const LONGEST_BODY: usize = 4 * 1024;
/// How long a browser may take to send its request, or to take the response.
const PATIENCE: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path without what follows a question mark.
    pub path: String,
    pub body: Vec<u8>,
}

impl Request {
    /// The fields of the form that the request posts, in its order.
    pub fn form(&self) -> Vec<(String, String)> {
        String::from_utf8_lossy(&self.body)
            .split('&')
            .filter(|field| !field.is_empty())
            .map(|field| {
                let (name, value) = field.split_once('=').unwrap_or((field, ""));
                (decode(name), decode(value))
            })
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    status: u16,
    reason: &'static str,
    /// The page to show, or where the browser finds it.
    content: Content,
}

#[derive(Debug, PartialEq, Eq)]
enum Content {
    Page(String),
    Elsewhere(String),
}

impl Response {
    pub fn page(html: String) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content: Content::Page(html),
        }
    }

    /// Sends the browser to `path` after a form was posted.
    pub fn see(path: &str) -> Self {
        Self {
            status: 303,
            reason: "See Other",
            content: Content::Elsewhere(path.to_owned()),
        }
    }

    /// Sends the browser to `address`, which may be on another host, whatever it asked for.
    pub fn found(address: &str) -> Self {
        Self {
            status: 302,
            reason: "Found",
            content: Content::Elsewhere(address.to_owned()),
        }
    }

    pub fn refused(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content: Content::Page(format!("<!doctype html><title>{reason}</title><p>{reason}</p>")),
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let (location, page) = match &self.content {
            Content::Page(html) => (String::new(), html.as_str()),
            Content::Elsewhere(path) => (format!("Location: {path}\r\n"), ""),
        };
        format!(
            "HTTP/1.1 {} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n{location}\r\n{page}",
            self.status,
            self.reason,
            page.len()
        )
        .into_bytes()
    }
}

/// Text as it stands in a page, where it cannot be taken for markup.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A name or a value of a posted form as it was typed.
fn decode(field: &str) -> String {
    let mut bytes = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let [first, after @ ..] = rest {
        rest = after;
        let byte = match first {
            b'+' => b' ',
            b'%' => match after.get(..2).and_then(|hex| core::str::from_utf8(hex).ok()) {
                Some(hex) => match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        rest = &after[2..];
                        byte
                    }
                    Err(_) => b'%',
                },
                None => b'%',
            },
            other => *other,
        };
        bytes.push(byte);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[derive(Debug)]
enum Unread {
    Io(io::Error),
    Refused(u16, &'static str),
}

impl From<io::Error> for Unread {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn read(stream: &mut impl Read) -> Result<Request, Unread> {
    let mut received = Vec::new();
    let head_end = loop {
        match received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            Some(end) if end <= LONGEST_HEAD => break end,
            _ => {}
        }
        if received.len() > LONGEST_HEAD {
            return Err(Unread::Refused(431, "Request Header Fields Too Large"));
        }
        let mut chunk = [0; 512];
        match stream.read(&mut chunk)? {
            0 => return Err(Unread::Io(io::ErrorKind::UnexpectedEof.into())),
            read => received.extend_from_slice(&chunk[..read]),
        }
    };

    let head = String::from_utf8_lossy(&received[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target)) = (first.next(), first.next()) else {
        return Err(Unread::Refused(400, "Bad Request"));
    };
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()
        .map_err(|_| Unread::Refused(400, "Bad Request"))?
        .unwrap_or(0);
    if length > LONGEST_BODY {
        return Err(Unread::Refused(413, "Content Too Large"));
    }

    let mut body = received[head_end + 4..].to_vec();
    while body.len() < length {
        let mut chunk = [0; 512];
        match stream.read(&mut chunk)? {
            0 => return Err(Unread::Io(io::ErrorKind::UnexpectedEof.into())),
            read => body.extend_from_slice(&chunk[..read]),
        }
    }
    body.truncate(length);

    Ok(Request {
        method: method.to_owned(),
        path: target.split('?').next().unwrap_or_default().to_owned(),
        body,
    })
}

fn answer(mut stream: TcpStream, handle: &mut impl FnMut(&Request) -> Response) -> io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let response = match read(&mut stream) {
        Ok(request) => handle(&request),
        Err(Unread::Refused(status, reason)) => Response::refused(status, reason),
        Err(Unread::Io(error)) => return Err(error),
    };
    stream.write_all(&response.to_bytes())
}

/// Serves the requests that arrive on `port` to `handle`, for as long as the device runs. A
/// browser that sends half a request or goes away is left behind.
pub fn serve(port: u16, mut handle: impl FnMut(&Request) -> Response) -> io::Result<Infallible> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))?;
    loop {
        let (stream, _) = listener.accept()?;
        let _ = answer(stream, &mut handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> Result<Request, Unread> {
        read(&mut text.as_bytes())
    }

    #[test]
    fn reads_a_request_for_a_page() {
        let request = request("GET /?again=1 HTTP/1.1\r\nHost: airplay-proxy.local\r\n\r\n").unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/");
        assert!(request.body.is_empty());
    }

    #[test]
    fn reads_a_posted_form() {
        let request = request(
            "POST /save HTTP/1.1\r\ncontent-length: 45\r\n\r\nname=Sala+cinema&receiver=AB%40Kitchen1&x=%ZZ and more",
        )
        .unwrap();
        assert_eq!(request.path, "/save");
        assert_eq!(
            request.form(),
            [
                ("name".to_owned(), "Sala cinema".to_owned()),
                ("receiver".to_owned(), "AB@Kitchen1".to_owned()),
                ("x".to_owned(), "%ZZ".to_owned()),
            ]
        );
    }

    #[test]
    fn reads_a_form_in_any_language() {
        let request = request("POST / HTTP/1.1\r\nContent-Length: 21\r\n\r\nname=Caff%C3%A8+%26+t").unwrap();
        assert_eq!(request.form(), [("name".to_owned(), "Caffè & t".to_owned())]);
    }

    #[test]
    fn refuses_what_is_too_large_or_not_a_request() {
        let large = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", LONGEST_BODY + 1);
        assert!(matches!(request(&large), Err(Unread::Refused(413, _))));
        let long = format!("GET / HTTP/1.1\r\nCookie: {}\r\n\r\n", "a".repeat(LONGEST_HEAD));
        assert!(matches!(request(&long), Err(Unread::Refused(431, _))));
        assert!(matches!(request("hello\r\n\r\n"), Err(Unread::Refused(400, _))));
        assert!(matches!(request("GET / HTTP/1.1\r\nContent-Length: x\r\n\r\n"), Err(Unread::Refused(400, _))));
        assert!(matches!(request("GET / HTTP/1.1\r\n"), Err(Unread::Io(_))));
        assert!(matches!(request("POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\nshort"), Err(Unread::Io(_))));
    }

    #[test]
    fn writes_a_page_and_a_way_to_another() {
        let page = String::from_utf8(Response::page("<p>è</p>".to_owned()).to_bytes()).unwrap();
        assert!(page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(page.contains("Content-Length: 9\r\n"));
        assert!(page.ends_with("\r\n\r\n<p>è</p>"));

        let see = String::from_utf8(Response::see("/").to_bytes()).unwrap();
        assert!(see.starts_with("HTTP/1.1 303 See Other\r\n"));
        assert!(see.contains("Location: /\r\n"));
        assert!(see.contains("Content-Length: 0\r\n"));

        let found = String::from_utf8(Response::found("http://192.168.71.1/").to_bytes()).unwrap();
        assert!(found.starts_with("HTTP/1.1 302 Found\r\n"));
        assert!(found.contains("Location: http://192.168.71.1/\r\n"));
    }

    #[test]
    fn escapes_text_for_a_page() {
        assert_eq!(escape(r#"<a href="x">Tom & 'Jerry'</a>"#), "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;");
    }

    #[test]
    fn serves_a_browser() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        std::thread::spawn(move || {
            serve(port, |request| match request.path.as_str() {
                "/" => Response::page("home".to_owned()),
                _ => Response::refused(404, "Not Found"),
            })
        });

        let ask = |path: &str| {
            let mut stream = (0..50)
                .find_map(|_| {
                    std::thread::sleep(Duration::from_millis(10));
                    TcpStream::connect(("127.0.0.1", port)).ok()
                })
                .unwrap();
            write!(stream, "GET {path} HTTP/1.1\r\n\r\n").unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        };
        assert!(ask("/").ends_with("\r\n\r\nhome"));
        assert!(ask("/else").starts_with("HTTP/1.1 404 Not Found\r\n"));
    }
}
