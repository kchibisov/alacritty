use std::ffi::c_void;
use std::io::Error;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicPtr, Ordering};

use polling::{Poller, Event};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};

use windows_sys::Win32::Foundation::{BOOLEAN, HANDLE};
use windows_sys::Win32::System::Threading::{
    RegisterWaitForSingleObject, UnregisterWait, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
};
use windows_sys::Win32::System::WindowsProgramming::INFINITE;

use crate::tty::ChildEvent;
use super::PTY_CHILD_EVENT_TOKEN;

struct ChildExitSender {
    sender: mpsc::Sender<ChildEvent>,
    poller: Arc<Poller>,
    packet: CompletionPacket,
}

/// WinAPI callback to run when child process exits.
extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: BOOLEAN) {
    if timed_out != 0 {
        return;
    }

    let event_tx: Box<_> = unsafe { Box::from_raw(ctx as *mut ChildExitSender) };
    let _ = event_tx.sender.send(ChildEvent::Exited);
    let _ = event_tx.poller.post(event_tx.packet);
}

pub struct ChildExitWatcher {
    wait_handle: AtomicPtr<c_void>,
    event_rx: mpsc::Receiver<ChildEvent>,
}

impl ChildExitWatcher {
    pub fn new(poller: &Arc<Poller>, child_handle: HANDLE) -> Result<ChildExitWatcher, Error> {
        let (event_tx, event_rx) = mpsc::channel();

        let mut wait_handle: HANDLE = 0;
        let sender_ref = Box::new(ChildExitSender {
            sender: event_tx,
            poller: poller.clone(),
            packet: CompletionPacket::new(Event::readable(PTY_CHILD_EVENT_TOKEN)),
        });

        let success = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                child_handle,
                Some(child_exit_callback),
                Box::into_raw(sender_ref).cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )
        };

        if success == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(ChildExitWatcher {
                wait_handle: AtomicPtr::from(wait_handle as *mut c_void),
                event_rx,
            })
        }
    }

    pub fn event_rx(&self) -> &mpsc::Receiver<ChildEvent> {
        &self.event_rx
    }
}

impl Drop for ChildExitWatcher {
    fn drop(&mut self) {
        unsafe {
            UnregisterWait(self.wait_handle.load(Ordering::Relaxed) as HANDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[test]
    pub fn event_is_emitted_when_child_exits() {
        const WAIT_TIMEOUT: Duration = Duration::from_millis(200);

        let poller = Arc::new(Poller::new().unwrap());

        let mut child = Command::new("cmd.exe").spawn().unwrap();
        let child_exit_watcher = ChildExitWatcher::new(&poller, child.as_raw_handle() as HANDLE).unwrap();

        child.kill().unwrap();

        // Poll for the event or fail with timeout if nothing has been sent.
        let mut events = vec![];
        poller.wait(&mut events, Some(WAIT_TIMEOUT)).unwrap();
        assert_eq!(events.iter().next().unwrap().key, PTY_CHILD_EVENT_TOKEN);
        // Verify that at least one `ChildEvent::Exited` was received.
        assert_eq!(child_exit_watcher.event_rx().try_recv(), Ok(ChildEvent::Exited));
    }
}
