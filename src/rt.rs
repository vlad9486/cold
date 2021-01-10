use core::{
    future::Future,
    task::{Context, Waker, RawWaker, RawWakerVTable},
    ptr,
    hash::Hash,
    fmt,
};
use std::{
    sync::{Arc, Weak, Mutex},
    collections::HashMap,
    io,
};
use futures::future::BoxFuture;
use crossbeam::deque::{Injector, Steal};
use mio::{Poll, Token, event::Source, Events, Interest};

pub trait Config {
    type TaskId: Eq + Hash + Clone + fmt::Display;
}

impl<T> Config for T
where
    T: Eq + Hash + Clone + fmt::Display,
{
    type TaskId = T;
}

pub struct Task<C>
where
    C: Config,
{
    id: C::TaskId,
    future: BoxFuture<'static, ()>,
}

enum WaitReason {
    Io(Token),
    // TODO:
    //Timer,
    //Signal,
    //OtherThread,
    //OtherTask,
}

pub enum Reg {
    Reg(Token, Interest),
    ReReg(Token, Interest),
    DeReg,
}

struct TaskContext {
    wait_reason: Option<WaitReason>,
}

pub struct Executor<C>
where
    C: Config,
{
    strong: Arc<ExecutorInner<C>>,
}

struct ExecutorInner<C>
where
    C: Config,
{
    injector: Injector<Task<C>>,
    poll: Mutex<Poll>,
    task_contexts: Mutex<HashMap<C::TaskId, TaskContext>>,
}

pub struct ExecutorRef<C>
where
    C: Config,
{
    weak: Weak<ExecutorInner<C>>,
    this: C::TaskId,
}

impl<C> Clone for ExecutorRef<C>
where
    C: Config,
{
    fn clone(&self) -> Self {
        ExecutorRef {
            weak: self.weak.clone(),
            this: self.this.clone(),
        }
    }
}

impl<C> ExecutorRef<C>
where
    C: Config,
{
    pub fn spawn<F, Fut>(&self, id: C::TaskId, f: F)
    where
        F: FnOnce(ExecutorRef<C>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let executor_ref = ExecutorRef {
            weak: self.weak.clone(),
            this: id.clone(),
        };
        let future = f(executor_ref);
        let task = Task {
            id: id.clone(),
            future: Box::pin(future),
        };
        let s = self.weak.upgrade().unwrap();
        s.injector.push(task);
        let context = TaskContext {
            wait_reason: None,
        };
        s.task_contexts.lock().unwrap().insert(id, context);
    }

    pub fn register<S>(
        &self,
        source: &mut S,
        reg: Reg,
    ) -> Result<(), io::Error>
    where
        S: Source + ?Sized + fmt::Debug,
    {
        let s = self.weak.upgrade().unwrap();
        let mut ctxs = s.task_contexts.lock().unwrap();
        let mut ctx = ctxs.get_mut(&self.this).unwrap();
        let poll = s.poll.lock().unwrap();
        let result = match reg {
            Reg::Reg(token, interests) => {
                ctx.wait_reason = Some(WaitReason::Io(token));
                poll.registry().register(source, token, interests)
            },
            Reg::ReReg(token, interests) => {
                ctx.wait_reason = Some(WaitReason::Io(token));
                poll.registry().register(source, token, interests)
            },
            Reg::DeReg => {
                ctx.wait_reason = None;
                poll.registry().deregister(source)
            },
        };
        result
    }

}

impl<C> Executor<C>
where
    C: Config,
{
    pub fn run<F, Fut>(id: C::TaskId, f: F) -> Result<(), io::Error>
    where
        F: FnOnce(ExecutorRef<C>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let rt = Self::new()?;
        let executor_ref = ExecutorRef {
            weak: Arc::downgrade(&rt.strong),
            this: id.clone(),
        };
        let future = f(executor_ref);
        let task = Task {
            id: id.clone(),
            future: Box::pin(future),
        };
        rt.strong.injector.push(task);
        let context = TaskContext {
            wait_reason: None,
        };
        rt.strong.task_contexts.lock().unwrap().insert(id, context);
        rt.run_inner()
    }

    fn new() -> Result<Self, io::Error> {
        Ok(Executor {
            strong: Arc::new(ExecutorInner {
                injector: Injector::new(),
                poll: Mutex::new(Poll::new()?),
                task_contexts: Mutex::new(HashMap::new()),
            }),
        })
    }

    fn run_inner(self) -> Result<(), io::Error> {
        static V_TABLE: RawWakerVTable =
            RawWakerVTable::new(|p| RawWaker::new(p, &V_TABLE), |_| (), |_| (), |_| ());

        let raw_waker = RawWaker::new(ptr::null(), &V_TABLE);
        let waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = Context::from_waker(&waker);

        let poll = &self.strong.poll;

        let mut waiting: HashMap<Token, Task<C>> = HashMap::new();
        let mut events = Events::with_capacity(128);

        loop {
            // traversal new tasks
            while !self.strong.injector.is_empty() {
                if let Steal::Success(mut task) = self.strong.injector.steal() {
                    //log::debug!("spawned new {}", task);
                    if task.future.as_mut().poll(&mut cx).is_pending() {
                        let ctxs = self.strong.task_contexts.lock().unwrap();
                        let ctx = ctxs.get(&task.id);
                        if let Some(TaskContext { wait_reason: Some(WaitReason::Io(t)) }) = ctx {
                            waiting.insert(t.clone(), task);
                        }
                    }
                }
            }

            // wait
            if !waiting.is_empty() {
                //log::debug!("poll {:?}", waiting);
                poll.lock().unwrap().poll(&mut events, None)?;
            } else {
                break Ok(());
            }

            // wake
            for event in events.into_iter() {
                if let Some(mut task) = waiting.remove(&event.token()) {
                    //log::debug!("try advance {}", task);
                    if task.future.as_mut().poll(&mut cx).is_pending() {
                        let ctxs = self.strong.task_contexts.lock().unwrap();
                        let ctx = ctxs.get(&task.id);
                        if let Some(TaskContext { wait_reason: Some(WaitReason::Io(t)) }) = ctx {
                            waiting.insert(t.clone(), task);
                        }
                    }
                }
            }
        }
    }
}
