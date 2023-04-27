// SPDX-License-Identifier: MIT OR Apache-2.0
//! Code for running a reader/writer on another thread while driving it through `polling`.

use piper::{pipe, Reader, Writer};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, Poller};

use std::io::prelude::*;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::{io, thread};

struct Interest {
    /// The event to send about completion.
    event: Event,

    /// The poller to send the event to.
    poller: Arc<Poller>,
}

/// Poll a reader in another thread.
pub struct UnblockedReader<R> {
    /// The event to send about completion.
    interest: Arc<Mutex<Option<Interest>>>,

    /// The pipe that we are reading from.
    pipe: Reader,

    /// We logically own the reader, but we don't actually use it.
    _reader: PhantomData<R>,
}

impl<R: Read + Send + 'static> UnblockedReader<R> {
    /// Spawn a new unblocked reader.
    pub fn new(mut source: R, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (reader, mut writer) = pipe(pipe_capacity);
        let interest = Arc::new(Mutex::<Option<Interest>>::new(None));

        // Spawn the reader thread.
        thread::Builder::new()
            .name("alacritty-tty-reader-thread".into())
            .spawn({
                let interest = interest.clone();
                move || {
                    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                    let mut context = Context::from_waker(&waker);

                    loop {
                        // Read from the reader into the pipe.
                        match writer.poll_fill(&mut context, &mut source) {
                            Poll::Ready(Ok(0)) => {
                                // Either the pipe is closed or the reader is at its EOF.
                                // In any case, we are done.
                                return;
                            },

                            Poll::Ready(Ok(_)) => {
                                // We read some bytes; wake up the poller.
                                let interest = interest.lock().unwrap();
                                if let Some(interest) = interest.as_ref() {
                                    if interest.event.readable {
                                        if let Err(e) = interest
                                            .poller
                                            .post(CompletionPacket::new(interest.event))
                                        {
                                            log::error!("error sending completion packet: {}", e);
                                        }
                                    }
                                }

                                // Keep reading.
                                continue;
                            },

                            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                                // We were interrupted; continue.
                                continue;
                            },

                            Poll::Ready(Err(e)) => {
                                log::error!("error writing to pipe: {}", e);
                                return;
                            },

                            Poll::Pending => {
                                // We are now waiting on the other end to advance. Park the
                                // thread until they do.
                                thread::park();
                            },
                        }
                    }
                }
            })
            .expect("failed to spawn reader thread");

        Self { interest, pipe: reader, _reader: PhantomData }
    }

    /// Register interest in the reader.
    pub fn register(&self, poller: &Arc<Poller>, event: Event) {
        let mut interest = self.interest.lock().unwrap();
        *interest = Some(Interest { event, poller: poller.clone() });
    }

    /// Deregister interest in the reader.
    pub fn deregister(&self) {
        let mut interest = self.interest.lock().unwrap();
        *interest = None;
    }

    /// Try to read from the reader.
    pub fn try_read(&mut self, buf: &mut [u8]) -> usize {
        self.pipe.try_drain(buf)
    }
}

impl<R: Read + Send + 'static> Read for UnblockedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.try_read(buf))
    }
}

/// Poll a writer in another thread.
pub struct UnblockedWriter<W> {
    /// The interest to send about completion.
    interest: Arc<Mutex<Option<Interest>>>,

    /// The pipe that we are writing to.
    pipe: Writer,

    /// We logically own the writer, but we don't actually use it.
    _reader: PhantomData<W>,
}

impl<W: Write + Send + 'static> UnblockedWriter<W> {
    /// Spawn a new unblocked writer.
    pub fn new(mut sink: W, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (mut reader, writer) = pipe(pipe_capacity);
        let interest = Arc::new(Mutex::<Option<Interest>>::new(None));

        // Spawn the writer thread.
        thread::Builder::new()
            .name("alacritty-tty-writer-thread".into())
            .spawn({
                let interest = interest.clone();
                move || {
                    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                    let mut context = Context::from_waker(&waker);

                    loop {
                        // Write from the pipe into the writer.
                        match reader.poll_drain(&mut context, &mut sink) {
                            Poll::Ready(Ok(0)) => {
                                // Either the pipe is closed or the writer is full.
                                // In any case, we are done.
                                return;
                            },

                            Poll::Ready(Ok(_)) => {
                                // We wrote some bytes; wake up the poller.
                                let interest = interest.lock().unwrap();
                                if let Some(interest) = interest.as_ref() {
                                    if interest.event.writable {
                                        if let Err(e) = interest
                                            .poller
                                            .post(CompletionPacket::new(interest.event))
                                        {
                                            log::error!("error sending completion packet: {}", e);
                                        }
                                    }
                                }

                                // Keep writing.
                                continue;
                            },

                            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                                // We were interrupted; continue.
                                continue;
                            },

                            Poll::Ready(Err(e)) => {
                                log::error!("error writing to pipe: {}", e);
                                return;
                            },

                            Poll::Pending => {
                                // We are now waiting on the other end to advance. Park the
                                // thread until they do.
                                thread::park();
                            },
                        }
                    }
                }
            })
            .expect("failed to spawn writer thread");

        Self { interest, pipe: writer, _reader: PhantomData }
    }

    /// Register interest in the writer.
    pub fn register(&self, poller: &Arc<Poller>, event: Event) {
        let mut interest = self.interest.lock().unwrap();
        *interest = Some(Interest { event, poller: poller.clone() });
    }

    /// Deregister interest in the writer.
    pub fn deregister(&self) {
        let mut interest = self.interest.lock().unwrap();
        *interest = None;
    }

    /// Try to write to the writer.
    pub fn try_write(&mut self, buf: &[u8]) -> usize {
        self.pipe.try_fill(buf)
    }
}

impl<W: Write + Send + 'static> Write for UnblockedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(self.try_write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        // Nothing to flush.
        Ok(())
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
