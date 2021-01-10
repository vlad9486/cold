use super::rt::Registry;

pin_project_lite::pin_project! {
    pub struct WithRegistry<T> {
        inner: T,
        registry: Registry,
        registered: bool,
    }
}

impl<T> WithRegistry<T> {
    pub fn new(inner: T, registry: &Registry) -> Self {
        WithRegistry {
            inner,
            registry: registry.clone(),
            registered: false,
        }
    }
}

#[cfg(unix)]
mod net_unix {
    use core::{
        pin::Pin,
        task::{Poll, Context},
    };
    use std::{
        io::{self, Read},
        net::{TcpListener, TcpStream, SocketAddr},
        os::unix::io::AsRawFd,
    };
    use futures::{Stream, AsyncRead};
    use mio::{unix::SourceFd, Interest};
    use super::WithRegistry;

    impl<T> WithRegistry<T>
    where
        T: AsRawFd,
    {
        pub fn deregister(&mut self) -> Result<(), io::Error> {
            if self.registered {
                let fd = self.inner.as_raw_fd();
                let mut source = SourceFd(&fd);
                self.registry.deregister(&mut source)
            } else {
                Ok(())
            }
        }
    }

    impl Stream for WithRegistry<TcpListener> {
        type Item = Result<(TcpStream, SocketAddr), io::Error>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let _ = cx;

            let fd = self.inner.as_raw_fd();
            let mut source = SourceFd(&fd);
            if !self.registered {
                self.registry
                    .register(&mut source, fd as _, Interest::READABLE)?;
                self.registered = true;
            } else {
                self.registry
                    .reregister(&mut source, fd as _, Interest::READABLE)?;
            }

            match self.inner.accept() {
                Ok(p) => Poll::Ready(Some(Ok(p))),
                Err(e) => match e.kind() {
                    io::ErrorKind::WouldBlock => Poll::Pending,
                    io::ErrorKind::UnexpectedEof => Poll::Ready(None),
                    _ => Poll::Ready(Some(Err(e))),
                },
            }
        }
    }

    impl<T> AsyncRead for WithRegistry<T>
    where
        T: AsRawFd + Read,
    {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<Result<usize, io::Error>> {
            let _ = cx;
            let this = self.project();

            let fd = this.inner.as_raw_fd();
            let mut source = SourceFd(&fd);
            if !*this.registered {
                this.registry
                    .register(&mut source, fd as _, Interest::READABLE)?;
                *this.registered = true;
            } else {
                this.registry
                    .reregister(&mut source, fd as _, Interest::READABLE)?;
            }

            match this.inner.read(buf) {
                Ok(read) => Poll::Ready(Ok(read)),
                Err(e) => match e.kind() {
                    io::ErrorKind::WouldBlock => Poll::Pending,
                    _ => Poll::Ready(Err(e)),
                },
            }
        }
    }
}
