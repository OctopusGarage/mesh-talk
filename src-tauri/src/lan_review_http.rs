use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout, timeout_at};

const MAX_REQUESTS: usize = 8;
const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024;
const REQUEST_TIME: Duration = Duration::from_secs(2);
const WRITE_TIME: Duration = Duration::from_secs(2);

pub(crate) async fn serve<A, F>(
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
    authorize: A,
) -> io::Result<()>
where
    A: Fn(SocketAddr, String, Instant) -> F + Send + Sync + 'static,
    F: Future<Output = Option<String>> + Send + 'static,
{
    let host = Arc::new(listener.local_addr()?.to_string());
    let authorize = Arc::new(authorize);
    let (stop_requests, request_shutdown) = watch::channel(false);
    let mut requests = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = stopped(&mut shutdown) => break Ok(()),
            Some(_) = requests.join_next(), if !requests.is_empty() => {}
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                let deadline = Instant::now() + REQUEST_TIME;
                if requests.len() >= MAX_REQUESTS {
                    // The listener retains no work or buffer for excess sockets.
                    drop(socket);
                    continue;
                }
                let authorize = authorize.clone();
                let host = host.clone();
                let shutdown = [shutdown.clone(), request_shutdown.clone()];
                requests.spawn(async move {
                    handle_connection(socket, peer, &host, shutdown, deadline, &*authorize).await;
                });
            }
        }
    };
    drop(listener);
    let _ = stop_requests.send(true);
    while requests.join_next().await.is_some() {}
    result
}

async fn handle_connection<A, F>(
    mut socket: impl AsyncRead + AsyncWrite + Unpin,
    peer: SocketAddr,
    host: &str,
    [mut listener_shutdown, mut request_shutdown]: [watch::Receiver<bool>; 2],
    deadline: Instant,
    authorize: A,
) where
    A: Fn(SocketAddr, String, Instant) -> F,
    F: Future<Output = Option<String>>,
{
    let response = tokio::select! {
        biased;
        _ = stopped(&mut listener_shutdown) => return,
        _ = stopped(&mut request_shutdown) => return,
        result = timeout_at(deadline, async {
            let target = read_target(&mut socket, host).await?;
            authorize(peer, target, deadline).await
        }) => result,
    };
    if let Ok(body) = response
        && Instant::now() < deadline
    {
        write_before_deadline(&mut socket, body).await;
    }
}

async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|value| *value).await;
}

async fn read_target(socket: &mut (impl AsyncRead + Unpin), host: &str) -> Option<String> {
    let mut header = [0; MAX_HEADER_BYTES];
    let mut len = 0;
    loop {
        let read = socket.read(&mut header[len..]).await.ok()?;
        if read == 0 {
            return None;
        }
        let previous_len = len;
        len += read;
        if let Some(offset) = header[previous_len.saturating_sub(3)..len]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let end = previous_len.saturating_sub(3) + offset;
            // Parse only the first request. Any additional bytes are discarded
            // when the connection closes, independent of TCP segmentation.
            return parse_target(&header[..end], host).map(str::to_owned);
        }
    }
}

fn parse_target<'a>(header: &'a [u8], host: &str) -> Option<&'a str> {
    let mut lines = std::str::from_utf8(header).ok()?.split("\r\n");
    let target = lines
        .next()?
        .strip_prefix("GET ")?
        .strip_suffix(" HTTP/1.1")?;
    if !target.starts_with('/') || !target.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }

    let mut valid_host = false;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^`_|~".contains(&byte))
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return None;
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("host") {
            if valid_host || value != host {
                return None;
            }
            valid_host = true;
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            || (name.eq_ignore_ascii_case("content-length") && value != "0")
        {
            return None;
        }
    }
    valid_host.then_some(target)
}

async fn write_before_deadline(socket: &mut (impl AsyncWrite + Unpin), body: Option<String>) {
    let _ = timeout(WRITE_TIME, write_response(socket, body)).await;
}

async fn write_response(
    socket: &mut (impl AsyncWrite + Unpin),
    body: Option<String>,
) -> io::Result<()> {
    let (status, body) = match body.filter(|body| body.len() <= MAX_BODY_BYTES) {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", String::new()),
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    async fn request(addr: SocketAddr, head: &str) -> String {
        let mut socket = TcpStream::connect(addr).await.unwrap();
        socket.write_all(head.as_bytes()).await.unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn serves_literal_text_and_drains_stalled_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, shutdown) = watch::channel(false);
        let server = tokio::spawn(serve(listener, shutdown, |_, target, _| async move {
            (target == "/valid").then(|| "<script>猫</script>".to_owned())
        }));

        let valid = request(
            addr,
            &format!("GET /valid HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
        )
        .await;
        assert!(valid.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(valid.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(valid.contains("Cache-Control: no-store\r\n"));
        assert!(valid.ends_with("\r\n\r\n<script>猫</script>"));

        for head in [
            format!("GET /unknown HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
            format!("POST /valid HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
            format!("GET /valid HTTP/1.1\r\nHost: {addr}\r\nHost: {addr}\r\n\r\n"),
            "GET /valid HTTP/1.1\r\nHost: other.example\r\n\r\n".to_owned(),
        ] {
            let denial = request(addr, &head).await;
            assert!(denial.starts_with("HTTP/1.1 404 Not Found\r\n"));
            assert!(denial.ends_with("\r\n\r\n"));
        }

        let mut idle = TcpStream::connect(addr).await.unwrap();
        let mut partial = TcpStream::connect(addr).await.unwrap();
        partial.write_all(b"GET /valid HTTP/1.1\r\n").await.unwrap();
        stop.send(true).unwrap();
        timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(TcpStream::connect(addr).await.is_err());
        for socket in [&mut idle, &mut partial] {
            let mut bytes = Vec::new();
            match socket.read_to_end(&mut bytes).await {
                Ok(_) => {}
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionReset),
            }
            assert!(bytes.is_empty());
        }
    }

    #[tokio::test]
    async fn first_request_framing_is_independent_of_trailing_tcp_bytes() {
        let headers = b"GET /valid HTTP/1.1\r\nHost: local:80\r\n\r\n";
        for joined in [false, true] {
            let (mut writer, mut reader) = tokio::io::duplex(128);
            writer.write_all(headers).await.unwrap();
            if joined {
                writer.write_all(b"trailing bytes").await.unwrap();
            }
            assert_eq!(
                read_target(&mut reader, "local:80").await.as_deref(),
                Some("/valid")
            );
            if !joined {
                writer.write_all(b"trailing bytes").await.unwrap();
            }
        }
        for header in [
            "GET /valid HTTP/1.1\r\nHost: local:80\r\nContent-Length: 1",
            "GET /valid HTTP/1.1\r\nHost: local:80\r\nTransfer-Encoding: chunked",
        ] {
            assert!(parse_target(header.as_bytes(), "local:80").is_none());
        }
    }

    #[tokio::test]
    async fn accepts_split_headers_and_exact_byte_limits() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        writer
            .write_all(b"GET /valid HTTP/1.1\r\nHost: local:80\r\n\r")
            .await
            .unwrap();
        let read = read_target(&mut reader, "local:80");
        tokio::pin!(read);
        assert!(
            std::future::poll_fn(|context| {
                use std::future::Future;
                std::task::Poll::Ready(read.as_mut().poll(context).is_pending())
            })
            .await
        );
        writer.write_all(b"\n").await.unwrap();
        assert_eq!(read.await.as_deref(), Some("/valid"));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, shutdown) = watch::channel(false);
        let payload = "猫".repeat(MAX_BODY_BYTES / 3) + "x";
        assert_eq!(payload.len(), MAX_BODY_BYTES);
        let expected = payload.clone();
        let server = tokio::spawn(serve(listener, shutdown, move |_, _, _| {
            std::future::ready(Some(payload.clone()))
        }));
        let prefix = format!("GET /valid HTTP/1.1\r\nHost: {addr}\r\nExtra: ");
        let head = format!(
            "{prefix}{}\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES - prefix.len() - 4)
        );
        assert_eq!(head.len(), MAX_HEADER_BYTES);
        let response = request(addr, &head).await;
        let (header, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(header.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(
            header
                .lines()
                .any(|line| line == format!("Content-Length: {MAX_BODY_BYTES}"))
        );
        assert_eq!(body, expected);
        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn limits_headers_bodies_and_authorization_time() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, shutdown) = watch::channel(false);
        let server = tokio::spawn(serve(listener, shutdown, |_, target, _| async move {
            match target.as_str() {
                "/wait" => std::future::pending().await,
                "/oversized" => Some("x".repeat(MAX_BODY_BYTES + 1)),
                _ => Some("ok".into()),
            }
        }));
        let head = |path| format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n");
        let excessive_header = format!(
            "GET /valid HTTP/1.1\r\nHost: {addr}\r\nExtra: {}\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES),
        );
        let mut oversized_socket = TcpStream::connect(addr).await.unwrap();
        oversized_socket
            .write_all(excessive_header.as_bytes())
            .await
            .unwrap();
        let mut denial = String::new();
        if let Err(error) = oversized_socket.read_to_string(&mut denial).await {
            // Closing with unread request bytes can reset the connection.
            assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        }
        assert!(denial.is_empty() || denial.starts_with("HTTP/1.1 404"));
        assert!(
            request(addr, &head("/oversized"))
                .await
                .starts_with("HTTP/1.1 404")
        );
        assert!(
            timeout(
                REQUEST_TIME + Duration::from_secs(1),
                request(addr, &head("/wait"))
            )
            .await
            .unwrap()
            .is_empty()
        );
        let mut partial = TcpStream::connect(addr).await.unwrap();
        partial.write_all(b"GET /valid HTTP/1.1\r\n").await.unwrap();
        let mut bytes = [0];
        let closed = timeout(
            REQUEST_TIME + Duration::from_secs(1),
            partial.read(&mut bytes),
        )
        .await
        .expect("the header deadline must close an incomplete request");
        assert!(
            matches!(closed, Ok(0))
                || closed.is_err_and(|error| error.kind() == io::ErrorKind::ConnectionReset)
        );
        assert!(request(addr, &head("/valid")).await.ends_with("\r\n\r\nok"));
        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_drains_an_authorized_backpressured_response() {
        let (sender, mut client) = tokio::io::duplex(64);
        let (stop, shutdown) = watch::channel(false);
        let peer: SocketAddr = "127.0.0.1:3210".parse().unwrap();
        let started = Instant::now();
        let task = tokio::spawn(handle_connection(
            sender,
            peer,
            "127.0.0.1:3210",
            [shutdown.clone(), shutdown],
            started + REQUEST_TIME,
            |_, _, _| async { Some("response text".into()) },
        ));
        client
            .write_all(b"GET /valid HTTP/1.1\r\nHost: 127.0.0.1:3210\r\n\r\n")
            .await
            .unwrap();

        // Receive only a prefix; the bounded transport fills and stalls.
        let mut prefix = [0; 4];
        client.read_exact(&mut prefix).await.unwrap();
        assert_eq!(&prefix, b"HTTP");
        stop.send(true).unwrap();
        timeout(WRITE_TIME + Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(started.elapsed() >= WRITE_TIME);

        let mut remaining = Vec::new();
        client.read_to_end(&mut remaining).await.unwrap();
        assert!(remaining.len() <= 64);
        assert!(!remaining.ends_with(b"response text"));
    }

    #[tokio::test]
    async fn rejects_saturation_and_cancels_pending_authorization() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, shutdown) = watch::channel(false);
        let (entered, mut entries) = tokio::sync::mpsc::channel(MAX_REQUESTS);
        let releases = Arc::new(tokio::sync::Semaphore::new(0));
        let authorization = releases.clone();
        let server = tokio::spawn(serve(listener, shutdown, move |_, _, _| {
            let entered = entered.clone();
            let releases = authorization.clone();
            async move {
                entered.send(()).await.unwrap();
                releases.acquire().await.unwrap().forget();
                Some("allowed".into())
            }
        }));

        let head = format!("GET /valid HTTP/1.1\r\nHost: {addr}\r\n\r\n");
        let mut sockets = Vec::new();
        for _ in 0..MAX_REQUESTS {
            let mut socket = TcpStream::connect(addr).await.unwrap();
            socket.write_all(head.as_bytes()).await.unwrap();
            sockets.push(socket);
        }
        for _ in 0..MAX_REQUESTS {
            timeout(Duration::from_secs(1), entries.recv())
                .await
                .unwrap()
                .unwrap();
        }

        let mut overflow = TcpStream::connect(addr).await.unwrap();
        if let Err(error) = overflow.write_all(head.as_bytes()).await {
            assert!(matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
            ));
        }
        let mut denial = Vec::new();
        if let Err(error) = timeout(Duration::from_secs(1), overflow.read_to_end(&mut denial))
            .await
            .unwrap()
        {
            assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        }
        assert!(denial.is_empty());
        assert!(
            entries.try_recv().is_err(),
            "overflow never reaches authorization"
        );

        releases.add_permits(MAX_REQUESTS);
        for mut socket in sockets {
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            assert!(response.ends_with("allowed"));
        }

        let mut pending = TcpStream::connect(addr).await.unwrap();
        pending.write_all(head.as_bytes()).await.unwrap();
        timeout(Duration::from_secs(1), entries.recv())
            .await
            .unwrap()
            .unwrap();
        stop.send(true).unwrap();
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut bytes = [0];
        assert_eq!(pending.read(&mut bytes).await.unwrap(), 0);
        assert!(entries.recv().await.is_none());
    }
}
