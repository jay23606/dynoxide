//! A mutex that a long-running holder can step aside from.
//!
//! `std::sync::Mutex` makes no promise about fairness, and in practice a thread
//! that unlocks and locks again straight away gets the lock back before a
//! waiting thread has even been woken. A TTL sweep releases the database lock
//! between pages so that other requests can run, which only helps if a request
//! that is waiting actually gets in.
//!
//! [`HandoffMutex`] counts the threads waiting for it by the generation they
//! arrived in. A holder about to step aside starts a [`HandOff`] while it still
//! holds the lock, which closes the current generation, then releases the lock
//! and waits until no thread from that generation or an earlier one is still
//! waiting. A thread that arrives after the hand-off started joins a later
//! generation, so it can neither use up an older waiter's turn nor keep the
//! holder waiting. Generations are never reused, so two hand-offs in flight at
//! once (two sweeps of one database) each wait for exactly the threads they
//! are owed.

use std::collections::VecDeque;
use std::sync::{Condvar, LockResult, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long a hand-off waits for the threads it is owed to take the lock.
///
/// The owed threads take the lock one after another, so this covers the time
/// each of them holds it as well as the time to wake. It is reached when an
/// owed request holds the lock for that long with another owed request queued
/// behind it, or when a waiter is stalled. The holder then goes ahead and
/// competes for the lock with the threads still queued, rather than waiting
/// on them indefinitely.
const HAND_OFF_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) struct HandoffMutex<T> {
    mutex: Mutex<T>,
    queue: Mutex<Queue>,
    /// Signalled when a generation empties while a hand-off is waiting.
    drained: Condvar,
    timeout: Duration,
}

/// Threads waiting for the lock, counted by the generation they arrived in.
#[derive(Default)]
struct Queue {
    /// The generation a thread arriving now joins. Only ever increases.
    current: u64,
    /// Threads inside [`HandoffMutex::lock`] that have not yet acquired, by
    /// generation, oldest first. Only generations with waiters are listed.
    waiting: VecDeque<(u64, usize)>,
    /// Hand-offs still waiting for their generations to empty.
    draining: usize,
}

impl Queue {
    fn join(&mut self) -> u64 {
        let generation = self.current;
        match self.waiting.back_mut() {
            Some((g, count)) if *g == generation => *count += 1,
            _ => self.waiting.push_back((generation, 1)),
        }
        generation
    }

    /// Remove one waiter from `generation`. Returns whether that emptied it.
    fn leave(&mut self, generation: u64) -> bool {
        let Some(at) = self.waiting.iter().position(|(g, _)| *g == generation) else {
            return false;
        };
        self.waiting[at].1 -= 1;
        if self.waiting[at].1 > 0 {
            return false;
        }
        self.waiting.remove(at);
        true
    }

    /// Whether a thread from `generation` or an earlier one is still waiting.
    fn owes(&self, generation: u64) -> bool {
        self.waiting.front().is_some_and(|(g, _)| *g <= generation)
    }
}

impl<T> HandoffMutex<T> {
    pub(crate) fn new(value: T) -> Self {
        Self::with_timeout(value, HAND_OFF_TIMEOUT)
    }

    fn with_timeout(value: T, timeout: Duration) -> Self {
        Self {
            mutex: Mutex::new(value),
            queue: Mutex::new(Queue::default()),
            drained: Condvar::new(),
            timeout,
        }
    }

    /// Lock, as [`Mutex::lock`] does, counting the wait against the current
    /// generation.
    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let place = Place::take(self);
        let result = self.mutex.lock();
        drop(place);
        result
    }

    /// Owe the lock to every thread waiting for it now. Call this while
    /// holding the lock, then release it and [`wait`](HandOff::wait).
    pub(crate) fn start_hand_off(&self) -> HandOff<'_, T> {
        let mut queue = self.queue();
        let closed = queue.current;
        queue.current += 1;
        queue.draining += 1;
        HandOff {
            owner: self,
            closed,
        }
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        // Nothing panics while holding it, but a poisoned count is still the
        // right count.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Threads waiting for the lock right now, in any generation.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.queue().waiting.iter().map(|(_, count)| count).sum()
    }

    /// Lock by spinning on `try_lock`, the way a holder that relocks the moment
    /// it can would, and never by queueing. A queued `lock` can be woken in
    /// arrival order, which would hide a hand-off that returned too early.
    #[cfg(test)]
    fn barge(&self) -> MutexGuard<'_, T> {
        loop {
            match self.mutex.try_lock() {
                Ok(guard) => return guard,
                Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
                Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
            }
        }
    }

    /// Count a waiter in the current generation that will never take the lock.
    #[cfg(test)]
    fn add_stalled_waiter(&self) {
        self.queue().join();
    }
}

/// A holder stepping aside for the threads that were waiting when it started.
pub(crate) struct HandOff<'a, T> {
    owner: &'a HandoffMutex<T>,
    /// The generation this hand-off closed: it is owed every thread from this
    /// generation or an earlier one.
    closed: u64,
}

impl<T> HandOff<'_, T> {
    /// With the lock released, block until every thread that was waiting when
    /// the hand-off started has taken the lock, or the timeout passes. Returns
    /// whether they all did.
    pub(crate) fn wait(self) -> bool {
        self.drain()
    }

    /// [`wait`](Self::wait), running `meanwhile` once the holder is waiting.
    #[cfg(test)]
    fn wait_while(self, meanwhile: impl FnOnce()) -> bool {
        meanwhile();
        self.drain()
    }

    fn drain(self) -> bool {
        let deadline = Instant::now() + self.owner.timeout;
        let mut queue = self.owner.queue();
        while queue.owes(self.closed) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            queue = self
                .owner
                .drained
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

impl<T> Drop for HandOff<'_, T> {
    fn drop(&mut self) {
        self.owner.queue().draining -= 1;
    }
}

/// One thread's place in the queue, given up when it acquires or unwinds, so
/// the count cannot keep a waiter that is not there.
struct Place<'a, T> {
    owner: &'a HandoffMutex<T>,
    generation: u64,
}

impl<'a, T> Place<'a, T> {
    fn take(owner: &'a HandoffMutex<T>) -> Self {
        let generation = owner.queue().join();
        Self { owner, generation }
    }
}

impl<T> Drop for Place<'_, T> {
    fn drop(&mut self) {
        let mut queue = self.owner.queue();
        if queue.leave(self.generation) && queue.draining > 0 {
            self.owner.drained.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    /// Generous, so a test that should drain never times out on a loaded
    /// machine. A test that expects the timeout sets its own.
    const PATIENT: Duration = Duration::from_secs(30);

    fn spin_until(what: impl Fn() -> bool) {
        while !what() {
            std::thread::yield_now();
        }
    }

    /// Start a thread that locks `m` and records `label`.
    fn locker(m: &Arc<HandoffMutex<Vec<&'static str>>>, label: &'static str) -> JoinHandle<()> {
        let m = Arc::clone(m);
        std::thread::spawn(move || m.lock().unwrap().push(label))
    }

    #[test]
    fn with_nobody_waiting_the_hand_off_returns_at_once() {
        let m = HandoffMutex::with_timeout((), PATIENT);
        let guard = m.lock().unwrap();
        let hand_off = m.start_hand_off();
        drop(guard);
        assert!(hand_off.wait());
    }

    #[test]
    fn two_queued_threads_both_take_the_lock_before_the_holder() {
        let m = Arc::new(HandoffMutex::with_timeout(Vec::new(), PATIENT));
        for _ in 0..50 {
            m.lock().unwrap().clear();
            let guard = m.lock().unwrap();
            let first = locker(&m, "first");
            let second = locker(&m, "second");
            spin_until(|| m.waiting() == 2);
            let hand_off = m.start_hand_off();
            drop(guard);
            assert!(hand_off.wait());

            let mut ran = m.barge().clone();
            ran.sort();
            assert_eq!(ran, ["first", "second"], "the holder locked again first");
            first.join().unwrap();
            second.join().unwrap();
        }
    }

    #[test]
    fn a_hand_off_is_not_done_until_every_owed_thread_has_taken_the_lock() {
        // Two threads are owed the lock, and one of them never takes it, so
        // the hand-off must run to its timeout rather than finish when the
        // other one gets in.
        for _ in 0..20 {
            let m = Arc::new(HandoffMutex::with_timeout(
                Vec::new(),
                Duration::from_millis(20),
            ));
            let guard = m.lock().unwrap();
            let real = locker(&m, "real");
            spin_until(|| m.waiting() == 1);
            m.add_stalled_waiter();
            let hand_off = m.start_hand_off();
            drop(guard);
            assert!(
                !hand_off.wait(),
                "the hand-off finished while an owed thread was still waiting"
            );
            real.join().unwrap();
        }
    }

    #[test]
    fn a_thread_arriving_while_the_holder_waits_cannot_use_up_a_queued_threads_turn() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let m = Arc::new(HandoffMutex::with_timeout(Vec::new(), PATIENT));
        for _ in 0..50 {
            m.lock().unwrap().clear();
            let go = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            // Already running when the holder starts waiting, so it can reach
            // the lock before the queued thread has even been woken.
            let late = {
                let (m, go, done) = (Arc::clone(&m), Arc::clone(&go), Arc::clone(&done));
                std::thread::spawn(move || {
                    spin_until(|| go.load(Ordering::SeqCst));
                    m.lock().unwrap().push("late");
                    done.store(true, Ordering::SeqCst);
                })
            };

            let guard = m.lock().unwrap();
            let queued = locker(&m, "queued");
            spin_until(|| m.waiting() == 1);
            let hand_off = m.start_hand_off();
            drop(guard);
            assert!(hand_off.wait_while(|| {
                go.store(true, Ordering::SeqCst);
                spin_until(|| done.load(Ordering::SeqCst));
            }));

            assert!(
                m.barge().contains(&"queued"),
                "the holder locked again before the queued thread had its turn"
            );
            queued.join().unwrap();
            late.join().unwrap();
        }
    }

    #[test]
    fn a_thread_arriving_during_the_hand_off_is_not_waited_for() {
        let m = Arc::new(HandoffMutex::with_timeout(Vec::new(), PATIENT));
        let guard = m.lock().unwrap();
        let queued = locker(&m, "queued");
        spin_until(|| m.waiting() == 1);
        let hand_off = m.start_hand_off();
        // A waiter in a later generation that never takes the lock.
        m.add_stalled_waiter();
        drop(guard);
        assert!(hand_off.wait(), "the hand-off waited on a later arrival");
        queued.join().unwrap();
    }

    #[test]
    fn overlapping_hand_offs_each_wait_only_for_the_threads_they_are_owed() {
        // Two sweeps on one database can each have a hand-off in flight at
        // once. Each is owed the threads waiting when it started, and neither
        // waits for a thread that arrived after both.
        let m = Arc::new(HandoffMutex::with_timeout(
            Vec::new(),
            Duration::from_secs(2),
        ));
        let guard = m.lock().unwrap();
        let first = locker(&m, "first");
        spin_until(|| m.waiting() == 1);
        let earlier = m.start_hand_off();
        let second = locker(&m, "second");
        spin_until(|| m.waiting() == 2);
        let later = m.start_hand_off();
        // Arrives after both hand-offs started, and never takes the lock.
        m.add_stalled_waiter();
        drop(guard);

        assert!(
            earlier.wait(),
            "the earlier hand-off waited on a thread that arrived after it"
        );
        assert!(
            later.wait(),
            "the later hand-off waited on a thread that arrived after it"
        );
        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn a_later_hand_off_is_also_owed_threads_from_earlier_generations() {
        // A thread already waiting when an earlier hand-off started is still
        // waiting when the later one starts, so the later one owes it too.
        let m = HandoffMutex::with_timeout((), Duration::from_millis(20));
        let guard = m.lock().unwrap();
        m.add_stalled_waiter();
        let earlier = m.start_hand_off();
        let later = m.start_hand_off();
        drop(guard);
        assert!(
            !later.wait(),
            "the later hand-off finished while a thread from an earlier generation was waiting"
        );
        drop(earlier);
    }

    #[test]
    fn a_waiter_that_never_arrives_cannot_stall_the_holder() {
        let m = HandoffMutex::with_timeout((), Duration::from_millis(20));
        let guard = m.lock().unwrap();
        m.add_stalled_waiter();
        let hand_off = m.start_hand_off();
        drop(guard);
        assert!(!hand_off.wait());
    }

    #[test]
    fn a_poisoned_lock_still_gives_up_its_place_in_the_queue() {
        let m = Arc::new(HandoffMutex::with_timeout((), PATIENT));
        let poisoner = {
            let m = Arc::clone(&m);
            std::thread::spawn(move || {
                let _guard = m.lock().unwrap();
                panic!("poison the lock");
            })
        };
        assert!(poisoner.join().is_err());

        assert!(m.lock().is_err());
        assert_eq!(m.waiting(), 0);
    }
}
