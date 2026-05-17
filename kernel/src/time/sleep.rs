// =============================================================================
// sleep.rs — Future pour dormir N millisecondes.
//
// Implémentation simple : liste chaînée protégée par mutex spin, chaque
// entrée = (deadline_ticks, waker). Chaque tick, on scanne et on wake les
// expirés. O(n) mais simple et suffisant pour quelques dizaines de timers.
//
// Upgrade : timer wheel ou min-heap si beaucoup de timers concurrents.
// =============================================================================

use alloc::collections::VecDeque;
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll, Waker},
};
use spin::Mutex;

struct TimerEntry {
    deadline: u64,
    waker: Waker,
    fired: bool,
}

static TIMERS: Mutex<VecDeque<alloc::sync::Arc<Mutex<TimerEntry>>>> = Mutex::new(VecDeque::new());
static NOW: AtomicU64 = AtomicU64::new(0);

/// Appelé par le handler timer à chaque tick.
pub(super) fn advance() {
    let now = NOW.fetch_add(1, Ordering::Relaxed) + 1;
    // On prend le lock en try : si quelqu'un est en train d'ajouter un timer,
    // on skip ce tick pour l'expiration (on réessaiera au prochain).
    let Some(mut timers) = TIMERS.try_lock() else { return };
    timers.retain(|entry| {
        let mut e = entry.lock();
        if !e.fired && e.deadline <= now {
            e.fired = true;
            e.waker.wake_by_ref();
            false  // retirer de la liste
        } else {
            true
        }
    });
}

pub fn sleep_ms(ms: u64) -> Sleep {
    let ticks = ms.saturating_mul(super::TICKS_PER_SEC) / 1000;
    let deadline = NOW.load(Ordering::Relaxed).saturating_add(ticks.max(1));
    Sleep { entry: None, deadline }
}

pub struct Sleep {
    entry: Option<alloc::sync::Arc<Mutex<TimerEntry>>>,
    deadline: u64,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        if NOW.load(Ordering::Relaxed) >= self.deadline {
            return Poll::Ready(());
        }
        // Premier poll : on s'enregistre dans la liste
        if self.entry.is_none() {
            let entry = alloc::sync::Arc::new(Mutex::new(TimerEntry {
                deadline: self.deadline,
                waker: cx.waker().clone(),
                fired: false,
            }));
            TIMERS.lock().push_back(entry.clone());
            self.entry = Some(entry);
        } else {
            // Re-poll : on met à jour le waker (il a pu changer)
            if let Some(e) = &self.entry {
                e.lock().waker = cx.waker().clone();
            }
        }
        Poll::Pending
    }
}
