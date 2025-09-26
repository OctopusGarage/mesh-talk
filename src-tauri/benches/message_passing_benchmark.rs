use criterion::{criterion_group, criterion_main, Criterion};
use mesh_talk::domain::message::{Message, MESSAGE_TYPE_CHAT, PROTOCOL_MAGIC, PROTOCOL_VERSION};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;

fn protocol_packet(message_type: u8, payload: &str) -> Vec<u8> {
    let mut packet = Vec::new();
    let magic = PROTOCOL_MAGIC;
    let version = PROTOCOL_VERSION;
    packet.extend_from_slice(&[
        (magic >> 24) as u8,
        (magic >> 16) as u8,
        (magic >> 8) as u8,
        magic as u8,
        version,
        message_type,
    ]);
    packet.extend_from_slice(payload.as_bytes());
    packet
}

fn benchmark_message_passing(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("message_passing", |b| {
        b.to_async(&rt).iter(|| async {
            let sender_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let receiver_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let receiver_addr = receiver_socket.local_addr().unwrap();

            let msg = Message::Chat {
                from: "benchmark_sender".to_string(),
                content: "hello".to_string(),
            };
            let payload = serde_json::to_string(&msg).unwrap();
            let packet = protocol_packet(MESSAGE_TYPE_CHAT, &payload);

            sender_socket.send_to(&packet, receiver_addr).await.unwrap();

            let mut buf = [0; 1024];
            receiver_socket.recv_from(&mut buf).await.unwrap();
        });
    });
}

criterion_group!(benches, benchmark_message_passing);
criterion_main!(benches);
