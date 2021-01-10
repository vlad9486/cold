use core::{
    future::Future,
    task::{Context, Waker, RawWaker, RawWakerVTable},
    ptr,
    marker::PhantomData,
    hash::Hash,
    fmt,
};
use std::{
    sync::{Arc, Weak},
    collections::HashMap,
    io,
    sync::Mutex,
};
use futures::future::BoxFuture;
use crossbeam::deque::{Injector, Steal};
use mio::{Poll, Token, event::Source, Events, Interest};

impl<T> Config for T
where
    T: Eq + Hash + fmt::Display,
{
    type TaskId = T;
}

pub trait Config {
    type TaskId: Eq + Hash + fmt::Display;
}

pub struct Executor<C>
where
    C: Config,
{
    spawner: Arc<SpawnerInner<C>>,
    registry: Arc<Mutex<RegistryInner>>,
}

#[derive(Clone)]
pub struct Registry {
    inner: Weak<Mutex<RegistryInner>>,
}

struct RegistryInner {
    poll: Poll,
    last_token: Option<Token>,
}

impl RegistryInner {
    fn new() -> Result<Self, io::Error> {
        Ok(RegistryInner {
            poll: Poll::new()?,
            last_token: None,
        })
    }

    fn last(&self) -> Option<Token> {
        self.last_token.clone()
    }
}

impl Registry {
    fn inner<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut RegistryInner) -> T,
    {
        let s = self.inner.upgrade().unwrap();
        let mut s = s.lock().unwrap();
        f(&mut s)
    }

    pub fn register<S>(
        &self,
        source: &mut S,
        token: usize,
        interests: Interest,
    ) -> Result<(), io::Error>
    where
        S: Source + ?Sized + fmt::Debug,
    {
        log::debug!("register {:?} {:?} {:?}", source, token, interests);

        self.inner(|s| {
            s.last_token = Some(Token(token));
            s.poll.registry().register(source, Token(token), interests)
        })
    }

    pub fn reregister<S>(
        &self,
        source: &mut S,
        token: usize,
        interests: Interest,
    ) -> Result<(), io::Error>
    where
        S: Source + ?Sized + fmt::Debug,
    {
        log::debug!("reregister {:?} {:?} {:?}", source, token, interests);

        self.inner(|s| {
            s.last_token = Some(Token(token));
            s.poll
                .registry()
                .reregister(source, Token(token), interests)
        })
    }

    pub fn deregister<S>(&self, source: &mut S) -> Result<(), io::Error>
    where
        S: Source + ?Sized + fmt::Debug,
    {
        log::debug!("deregister {:?}", source);

        self.inner(|s| {
            s.last_token = None;
            s.poll.registry().deregister(source)
        })
    }
}

pub struct Task<C>
where
    C: Config,
{
    id: C::TaskId,
    future: BoxFuture<'static, ()>,
}

impl<C> fmt::Display for Task<C>
where
    C: Config,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task({})", self.id)
    }
}

impl<C> fmt::Debug for Task<C>
where
    C: Config,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Task").field(&self.id.to_string()).finish()
    }
}

pub struct Spawner<C>
where
    C: Config,
{
    inner: Weak<SpawnerInner<C>>,
    registry: Weak<Mutex<RegistryInner>>,
}

struct SpawnerInner<C>
where
    C: Config,
{
    injector: Injector<Task<C>>,
    phantom_data: PhantomData<C>,
}

impl<C> SpawnerInner<C>
where
    C: Config,
{
    fn new() -> Self {
        SpawnerInner {
            injector: Injector::new(),
            phantom_data: PhantomData,
        }
    }
}

impl<C> Clone for Spawner<C>
where
    C: Config,
{
    fn clone(&self) -> Self {
        Spawner {
            inner: self.inner.clone(),
            registry: self.registry.clone(),
        }
    }
}

impl<C> Spawner<C>
where
    C: Config,
{
    pub fn spawn<F, Fut>(&self, id: C::TaskId, f: F)
    where
        F: FnOnce(Spawner<C>, Registry) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let future = f(
            self.clone(),
            Registry {
                inner: self.registry.clone(),
            },
        );
        let task = Task {
            id,
            future: Box::pin(future),
        };
        self.inner.upgrade().unwrap().injector.push(task);
    }
}

impl<C> Executor<C>
where
    C: Config,
{
    pub fn run<F, Fut>(id: C::TaskId, f: F) -> Result<(), io::Error>
    where
        F: FnOnce(Spawner<C>, Registry) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let rt = Self::new()?;
        rt.spawner().spawn(id, f);
        rt.run_inner()
    }

    fn new() -> Result<Self, io::Error> {
        Ok(Executor {
            spawner: Arc::new(SpawnerInner::new()),
            registry: Arc::new(Mutex::new(RegistryInner::new()?)),
        })
    }

    fn spawner(&self) -> Spawner<C> {
        Spawner {
            inner: Arc::downgrade(&self.spawner),
            registry: Arc::downgrade(&self.registry),
        }
    }

    fn run_inner(self) -> Result<(), io::Error> {
        static V_TABLE: RawWakerVTable =
            RawWakerVTable::new(|p| RawWaker::new(p, &V_TABLE), |_| (), |_| (), |_| ());

        let raw_waker = RawWaker::new(ptr::null(), &V_TABLE);
        let waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = Context::from_waker(&waker);

        let reg = self.registry.as_ref();

        let mut waiting: HashMap<Token, Task<C>> = HashMap::new();
        let mut events = Events::with_capacity(128);

        loop {
            // traversal new tasks
            while !self.spawner.injector.is_empty() {
                if let Steal::Success(mut task) = self.spawner.injector.steal() {
                    log::debug!("spawned new {}", task);
                    if task.future.as_mut().poll(&mut cx).is_pending() {
                        // TODO: fix race condition
                        if let Some(token) = reg.lock().unwrap().last() {
                            waiting.insert(token, task);
                        }
                    }
                }
            }

            // wait
            if !waiting.is_empty() {
                log::debug!("poll {:?}", waiting);
                reg.lock().unwrap().poll.poll(&mut events, None)?;
            } else {
                break Ok(());
            }

            // wake
            for event in events.into_iter() {
                if let Some(mut task) = waiting.remove(&event.token()) {
                    log::debug!("try advance {}", task);
                    if task.future.as_mut().poll(&mut cx).is_pending() {
                        // TODO: fix race condition
                        if let Some(token) = reg.lock().unwrap().last() {
                            waiting.insert(token, task);
                        }
                    }
                }
            }
        }
    }
}
