use core::{time::Duration, str};
use std::{
    thread,
    io::{self, Write},
    net::{TcpListener, TcpStream},
};
use futures::{AsyncReadExt, StreamExt};
use cold::{rt::Executor, net::WithRegistry};

#[test]
fn nop() -> io::Result<()> {
    Executor::<usize>::run(0, move |_| async {})
}

#[test]
fn nested() -> io::Result<()> {
    Executor::<usize>::run(0, move |_| async {
        async { async { println!("hello world") }.await }.await
    })
}

#[test]
fn nested_spawn() -> io::Result<()> {
    Executor::<usize>::run(0, move |executor| async move {
        println!("hello outer world");
        executor.spawn(1, move |_| async {
            println!("hello inner world");
        });
    })
}

#[test]
#[cfg(unix)]
fn tcp_server() -> io::Result<()> {
    simple_logging::log_to_stderr(log::LevelFilter::Debug);

    thread::spawn(move || {
        for i in 0..4 {
            thread::sleep(Duration::from_millis(500));
            let mut stream = TcpStream::connect("127.0.0.1:9000").unwrap();
            stream.write_all(b"hello world: ").unwrap();
            stream.write_all(&['0' as u8 + i]).unwrap();
        }
    });

    Executor::<usize>::run(0, move |executor| async move {
        let listener = TcpListener::bind("127.0.0.1:9000").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut listener = WithRegistry::new(listener, &executor);
        let mut id = 1;
        while let Some(p) = listener.next().await {
            let (stream, address) = p.unwrap();
            println!("{:?}", address);

            executor.spawn(id, move |executor| async move {
                let mut stream = WithRegistry::new(stream, &executor);
                let mut buf = [0; 0x100];
                let read = stream.read(&mut buf).await.unwrap();
                println!("{}", str::from_utf8(&buf[..read]).unwrap());
            });
            id += 1;
            if id > 4 {
                break;
            }
        }
    })
}
