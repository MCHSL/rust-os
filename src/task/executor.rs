use super::{Task, TaskId};
use alloc::task::Wake;
use alloc::{collections::BTreeMap, sync::Arc};
use conquer_once::spin::OnceCell;
use core::task::{Context, Poll, Waker};
use crossbeam_queue::ArrayQueue;

pub struct Executor {
    tasks: BTreeMap<TaskId, Task>,
    task_queue: Arc<ArrayQueue<TaskId>>,
    waker_cache: BTreeMap<TaskId, Waker>,
    incoming_tasks: Arc<ArrayQueue<Task>>,
}

#[derive(Clone)]
pub struct TaskSpawner {
    task_queue: Arc<ArrayQueue<Task>>,
}

impl TaskSpawner {
    fn new(task_queue: Arc<ArrayQueue<Task>>) -> Self {
        Self { task_queue }
    }

    pub fn spawn(&self, task: Task) {
        self.task_queue.push(task).expect("Failed to send task");
    }
}

pub static TASK_SPAWNER: OnceCell<TaskSpawner> = OnceCell::uninit();

pub fn spawn_task(task: Task) {
    let spawner = TASK_SPAWNER.get().expect("Executor not created");
    spawner.spawn(task);
}

impl Executor {
    pub fn new() -> Self {
        let incoming_tasks = Arc::new(ArrayQueue::new(100));
        TASK_SPAWNER.init_once(|| TaskSpawner::new(incoming_tasks.clone()));
        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(100)),
            waker_cache: BTreeMap::new(),
            incoming_tasks,
        }
    }

    pub fn spawn(&mut self, task: Task) {
        let task_id = task.id;
        if self.tasks.insert(task.id, task).is_some() {
            panic!("task with same ID already in tasks");
        }
        self.task_queue.push(task_id).expect("queue full");
    }
}

impl Executor {
    fn run_ready_tasks(&mut self) {
        // destructure `self` to avoid borrow checker errors
        let Self {
            tasks,
            task_queue,
            waker_cache,
            incoming_tasks: _,
        } = self;

        while let Ok(task_id) = task_queue.pop() {
            let task = match tasks.get_mut(&task_id) {
                Some(task) => task,
                None => continue, // task no longer exists
            };
            let waker = waker_cache
                .entry(task_id)
                .or_insert_with(|| TaskWaker::new(task_id, task_queue.clone()));
            let mut context = Context::from_waker(waker);
            match task.poll(&mut context) {
                Poll::Ready(()) => {
                    // task done -> remove it and its cached waker
                    tasks.remove(&task_id);
                    waker_cache.remove(&task_id);
                }
                Poll::Pending => {}
            }
        }
    }

    fn add_incoming_tasks(&mut self) {
        while let Ok(task) = self.incoming_tasks.pop() {
            self.spawn(task);
        }
    }

    pub fn run(&mut self) -> ! {
        loop {
            self.add_incoming_tasks();
            self.run_ready_tasks();
            self.sleep_if_idle();
        }
    }

    fn sleep_if_idle(&self) {
        use x86_64::instructions::interrupts::{self, enable_and_hlt};

        interrupts::disable();
        if self.task_queue.is_empty() {
            enable_and_hlt();
        } else {
            interrupts::enable();
        }
    }

    pub fn spawner(&self) -> TaskSpawner {
        TaskSpawner::new(self.incoming_tasks.clone())
    }
}

struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
}

impl TaskWaker {
    fn wake_task(&self) {
        self.task_queue.push(self.task_id).expect("task_queue full");
    }

    #[allow(clippy::new_ret_no_self)]
    fn new(task_id: TaskId, task_queue: Arc<ArrayQueue<TaskId>>) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            task_id,
            task_queue,
        }))
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task();
    }
}
