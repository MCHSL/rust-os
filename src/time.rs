use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use alloc::{sync::Arc, vec::Vec};
use futures_util::{task::AtomicWaker, Future};
use generic_once_cell::Lazy;
use spin::Mutex;
use x86_64::{
    instructions::{interrupts, port::Port},
    structures::idt::InterruptStackFrame,
};

use crate::{
    interrupts::{InterruptIndex, PICS},
    print, println,
    task::{executor::TASK_SPAWNER, Task},
};

pub const PIT_FREQUENCY: f64 = 3_579_545.0 / 3.0; // 1_193_181.666 Hz
pub const PIT_DIVIDER: usize = 1193;
pub const PIT_INTERVAL: f64 = (PIT_DIVIDER as f64) / PIT_FREQUENCY;

static CLOCK: AtomicUsize = AtomicUsize::new(0);

pub fn elapsed(start_tick: usize) -> f64 {
    let interval = CLOCK.load(Ordering::Relaxed) - start_tick;
    interval as f64 * PIT_INTERVAL
}

pub fn time() -> f64 {
    CLOCK.load(Ordering::Relaxed) as f64 * PIT_INTERVAL
}

pub fn time_ms() -> i64 {
    (time() * 1000.0) as i64
}

pub fn time_us() -> i64 {
    (time() * 1000000.0) as i64
}

struct SleepInner {
    waker: AtomicWaker,
    wake_at: f64,
}

#[derive(Clone)]
pub struct Sleep {
    _inner: Arc<SleepInner>,
}

impl Sleep {
    fn new(wake_at: f64) -> Self {
        Self {
            _inner: Arc::new(SleepInner {
                waker: AtomicWaker::new(),
                wake_at,
            }),
        }
    }
}

impl Future for Sleep {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let time = time();
        if time >= self._inner.wake_at {
            return Poll::Ready(());
        }

        self._inner.waker.register(cx.waker());
        Poll::Pending
    }
}

pub struct Yield {
    polled: AtomicBool,
}

impl Future for Yield {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let polled = self.polled.load(Ordering::Relaxed);
        if polled {
            Poll::Ready(())
        } else {
            self.polled.store(true, Ordering::Relaxed);
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

static SLEEPERS: Lazy<Mutex<()>, Mutex<Vec<Sleep>>> = Lazy::new(|| Mutex::new(Vec::new()));

pub fn sleep(duration: Duration) -> Sleep {
    let start_time = time();
    let end_time = start_time + duration.as_secs_f64();
    let sleepster = Sleep::new(end_time);
    interrupts::without_interrupts(|| {
        SLEEPERS.lock().push(sleepster.clone());
    });

    sleepster
}

pub fn yield_now() -> Yield {
    Yield {
        polled: AtomicBool::new(false),
    }
}

fn wake_sleepers() {
    let time = time();
    SLEEPERS.lock().retain(|sleeper| {
        if sleeper._inner.wake_at <= time {
            sleeper._inner.waker.wake();
            false
        } else {
            true
        }
    });
}

pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    CLOCK.fetch_add(1, Ordering::Relaxed);

    //print!(".");
    wake_sleepers();

    /*TASK_SPAWNER
    .get()
    .expect("TASK_SPAWNER not initialized")
    .spawn(Task::new(wake_sleepers()));*/

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

pub fn set_pit_frequency_divider(divider: u16, channel: u8) {
    interrupts::without_interrupts(|| {
        let bytes = divider.to_le_bytes();
        let mut cmd: Port<u8> = Port::new(0x43);
        let mut data: Port<u8> = Port::new(0x40 + channel as u16);
        let operating_mode = 6; // Square wave generator
        let access_mode = 3; // Lobyte + Hibyte
        unsafe {
            cmd.write((channel << 6) | (access_mode << 4) | operating_mode);
            data.write(bytes[0]);
            data.write(bytes[1]);
        }
    });
}

pub extern "x86-interrupt" fn rtc_interrupt_handler(stack_frame: InterruptStackFrame) {
    print!("+");
    static SHOWN: AtomicBool = AtomicBool::new(false);
    let val = SHOWN.load(Ordering::Relaxed);
    if !val {
        println!("{:?}", stack_frame);
        SHOWN.store(true, Ordering::Relaxed);
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::RealTimeClock.as_u8());
    }
}
