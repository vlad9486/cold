use super::rt::{ExecutorRef, Config};

pin_project_lite::pin_project! {
    pub struct WithRegistry<T, C>
    where
        C: Config,
    {
        inner: T,
        executor: ExecutorRef<C>,
        registered: bool,
    }
}

impl<T, C> WithRegistry<T, C>
where
    C: Config,
{
    pub fn new(inner: T, executor: &ExecutorRef<C>) -> Self {
        WithRegistry {
            inner,
            executor: executor.clone(),
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
    use mio::{unix::SourceFd, Token, Interest};
    use super::{WithRegistry, super::rt::{Config, Reg}};

    impl<T, C> WithRegistry<T, C>
    where
        T: AsRawFd,
        C: Config,
    {
        pub fn deregister(&mut self) -> Result<(), io::Error> {
            if self.registered {
                let fd = self.inner.as_raw_fd();
                let mut source = SourceFd(&fd);
                self.executor.register(&mut source, Reg::DeReg)
            } else {
                Ok(())
            }
        }
    }

    impl<C> Stream for WithRegistry<TcpListener, C>
    where
        C: Config,
    {
        type Item = Result<(TcpStream, SocketAddr), io::Error>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let _ = cx;

            let fd = self.inner.as_raw_fd();
            let mut source = SourceFd(&fd);
            if !self.registered {
                self.executor
                    .register(&mut source, Reg::Reg(Token(fd as _), Interest::READABLE))?;
                self.registered = true;
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

    impl<T, C> AsyncRead for WithRegistry<T, C>
    where
        T: AsRawFd + Read,
        C: Config,
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
                this.executor
                    .register(&mut source, Reg::Reg(Token(fd as _), Interest::READABLE))?;
                *this.registered = true;
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
