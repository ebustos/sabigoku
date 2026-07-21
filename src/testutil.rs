//! Test-only HTTP mock shared by the transport tests (anilist, providers).

/// One-shot HTTP responder; drains the request, writes `response`, closes.
pub fn serve_once(response: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = std::io::Read::read(&mut sock, &mut buf);
        let _ = std::io::Write::write_all(&mut sock, &response);
    });
    format!("http://{addr}/")
}

/// Like [`serve_once`] but hands back the raw request bytes it received, so a
/// test can assert on the headers/body the client sent (e.g. the bearer).
pub fn serve_once_capture(response: Vec<u8>) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
        let _ = tx.send(buf[..n].to_vec());
        let _ = std::io::Write::write_all(&mut sock, &response);
    });
    (format!("http://{addr}/"), rx)
}

pub fn response_with_body(status: &str, body: &[u8]) -> Vec<u8> {
    let mut r = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(body);
    r
}
