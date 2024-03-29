use criterion::{criterion_group, criterion_main, Criterion};
use ddtelemetry::ipc::example_interface::{
    ExampleInterfaceRequest, ExampleInterfaceResponse, ExampleServer, ExampleTransport,
};
use std::{
    os::unix::net::UnixStream as StdUnixStream,
    thread::{self},
};
use tokio::{net::UnixStream, runtime};


fn criterion_benchmark(c: &mut Criterion) {
    let (sock_a, sock_b) = StdUnixStream::pair().unwrap();

    let worker = thread::spawn(move || {
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = rt.enter();
        sock_a.set_nonblocking(true).unwrap();
        let socket = UnixStream::from_std(sock_a).unwrap();
        let server = ExampleServer::default();

        rt.block_on(server.accept_connection(socket));
    });

    let mut transport = ExampleTransport::from(sock_b);
    transport.set_nonblocking(false).unwrap();

    c.bench_function("write only interface", |b| {
        b.iter(|| transport.send_ignore_response(ExampleInterfaceRequest::Ping {}).unwrap())
    });

    c.bench_function("two way interface", |b| {
        b.iter(|| transport.send(ExampleInterfaceRequest::ReqCnt {}).unwrap())
    });

    let requests_received = match transport.send(ExampleInterfaceRequest::ReqCnt {}).unwrap() {
        ExampleInterfaceResponse::ReqCnt(cnt) => cnt,
        _ => panic!("shouldn't happen"),
    };


    println!("Total requests handled: {}", requests_received);

    drop(transport);
    worker.join().unwrap();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
