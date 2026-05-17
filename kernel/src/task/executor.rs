// =============================================================================
// executor.rs — ordonnanceur async coopératif (v2).
//
// Nouveautés vs v1 :
//   - spawn_queue global : on peut spawner depuis n'importe où (y compris
//     depuis une commande shell, à chaud)
//   - task_count exposé pour la commande `ps`
//   - intégration avec les sleeps (time::sleep) via la machinerie Waker
//
// Modèle inchangé :
//   * tasks indexées par TaskId
//   * task_queue = IDs à poll
//   * waker_cache = Waker réutilisables
//   * idle = HLT (enable_and_hlt pour éviter race)
// =============================================================================

use super::{Task, TaskId};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    task::Wake,
};
use core::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use crossbeam_queue::ArrayQueue;
use spin::Mutex;

/// Queue de spawn globale. Remplie par `spawn()` depuis n'importe où,
/// drainée par l'executor entre deux phases de poll.
static SPAWN_QUEUE: Mutex<Option<alloc::collections::VecDeque<Task>>> = Mutex::new(None);
static TASK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Spawn global : accessible depuis n'importe où (shell, ISR via async).
pub fn spawn<F>(future: F)
where F: Future<Output = ()> + Send + 'static
{
    let task = Task::new(future);
    let mut guard = SPAWN_QUEUE.lock();
    if let Some(queue) = guard.as_mut() {
        queue.push_back(task);
    }
    // Si SPAWN_QUEUE pas encore init (avant Executor::new), on drop —
    // pas grave puisque personne ne peut encore appeler spawn() avant
    // que l'executor démarre (il est appelé depuis _start qui tourne
    // le thread principal).
}

pub fn task_count() -> usize { TASK_COUNT.load(Ordering::Relaxed) }

pub struct Executor {
    tasks: BTreeMap<TaskId, Task>,
    task_queue: Arc<ArrayQueue<TaskId>>,
    queued_tasks: Arc<Mutex<BTreeSet<TaskId>>>,
    waker_cache: BTreeMap<TaskId, Waker>,
}

impl Executor {
    pub fn new() -> Self {
        *SPAWN_QUEUE.lock() = Some(alloc::collections::VecDeque::new());
        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(256)),
            queued_tasks: Arc::new(Mutex::new(BTreeSet::new())),
            waker_cache: BTreeMap::new(),
        }
    }

    pub fn spawn(&mut self, task: Task) {
        let id = task.id();
        if self.tasks.insert(id, task).is_some() {
            panic!("task id {} collision", id.0);
        }
        TASK_COUNT.fetch_add(1, Ordering::Relaxed);
        queue_task(id, &self.task_queue, &self.queued_tasks).expect("task_queue plein");
    }

    pub fn run(&mut self) -> ! {
        loop {
            self.drain_spawn_queue();
            self.run_ready_tasks();
            self.sleep_if_idle();
        }
    }

    fn drain_spawn_queue(&mut self) {
        let Some(mut queue) = SPAWN_QUEUE.try_lock() else { return };
        if let Some(q) = queue.as_mut() {
            while let Some(task) = q.pop_front() {
                let id = task.id();
                if self.tasks.insert(id, task).is_none() {
                    TASK_COUNT.fetch_add(1, Ordering::Relaxed);
                    let _ = queue_task(id, &self.task_queue, &self.queued_tasks);
                }
            }
        }
    }

    fn run_ready_tasks(&mut self) {
        let Self { tasks, task_queue, queued_tasks, waker_cache } = self;
        while let Some(id) = task_queue.pop() {
            queued_tasks.lock().remove(&id);
            let task = match tasks.get_mut(&id) {
                Some(t) => t, None => continue,
            };
            let waker = waker_cache
                .entry(id)
                .or_insert_with(|| TaskWaker::new(id, task_queue.clone(), queued_tasks.clone()));
            let mut cx = Context::from_waker(waker);
            match task.poll(&mut cx) {
                Poll::Ready(()) => {
                    tasks.remove(&id);
                    waker_cache.remove(&id);
                    TASK_COUNT.fetch_sub(1, Ordering::Relaxed);
                }
                Poll::Pending => {}
            }
        }
    }

    fn sleep_if_idle(&self) {
        use x86_64::instructions::interrupts::{self, enable_and_hlt};
        interrupts::disable();
        // Re-vérifie qu'aucun nouveau job n'a été ajouté juste avant
        let has_spawn = SPAWN_QUEUE.try_lock()
            .and_then(|g| g.as_ref().map(|q| !q.is_empty()))
            .unwrap_or(false);
        if self.task_queue.is_empty() && !has_spawn {
            enable_and_hlt();
        } else {
            interrupts::enable();
        }
    }
}

struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
    queued_tasks: Arc<Mutex<BTreeSet<TaskId>>>,
}

impl TaskWaker {
    fn new(
        task_id: TaskId,
        task_queue: Arc<ArrayQueue<TaskId>>,
        queued_tasks: Arc<Mutex<BTreeSet<TaskId>>>,
    ) -> Waker {
        Waker::from(Arc::new(TaskWaker { task_id, task_queue, queued_tasks }))
    }
    fn wake_task(&self) {
        let _ = queue_task(self.task_id, &self.task_queue, &self.queued_tasks);
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>)         { self.wake_task(); }
    fn wake_by_ref(self: &Arc<Self>) { self.wake_task(); }
}

// Empêche l'inlining de Box::pin pour que Task::new reste ABI-compatible
// si on change de stratégie d'allocation plus tard.
#[inline(never)]
fn _no_inline<T>(x: Box<T>) -> Box<T> { x }

fn queue_task(
    id: TaskId,
    task_queue: &Arc<ArrayQueue<TaskId>>,
    queued_tasks: &Arc<Mutex<BTreeSet<TaskId>>>,
) -> Result<(), TaskId> {
    {
        let mut queued = queued_tasks.lock();
        if !queued.insert(id) {
            return Ok(());
        }
    }

    if task_queue.push(id).is_ok() {
        Ok(())
    } else {
        queued_tasks.lock().remove(&id);
        Err(id)
    }
}
