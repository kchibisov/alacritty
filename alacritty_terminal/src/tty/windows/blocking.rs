// SPDX-License-Identifier: MIT OR Apache-2.0
//! Code for running a reader/writer on another thread while driving it through `polling`.

use piper::{pipe, Reader, Writer};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, Poller};

use std::io::{self, prelude::*};
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};

/// Poll a reader in another thread.
pub struct UnblockedReader<R> {
    /// The thread that is running the reader.
    thread: JoinHandle<()>,

    /// The packet that we are waiting for.
    packet: CompletionPacket,

    /// The pipe that we are reading from.
    pipe: Reader,

    /// We logically own the reader, but we don't actually use it.
    _reader: PhantomData<R>,
}

impl<R: Read + Send + 'static> UnblockedReader<R> {
    /// Spawn a new unblocked reader.
    pub fn new(mut source: R, poller: &Arc<Poller>, event: Event, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (reader, mut writer) = pipe(pipe_capacity);
        let packet = CompletionPacketButSend(CompletionPacket::new(event));

        // Spawn the reader thread.
        let handle = thread::Builder::new()
            .name("alacritty-tty-reader-thread".into())
            .spawn({
                let poller = poller.clone();
                let packet2 = packet.clone();
                move || {
                    let thread = thread::current();
                    let waker = Waker::from(Arc::new(ThreadWaker(thread.clone())));
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
                                packet2.clone().post(&poller);

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

        Self { thread: handle, packet: packet.0, pipe: reader, _reader: PhantomData }
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
    /// The thread that is running the writer.
    thread: JoinHandle<()>,

    /// The packet that we are waiting for.
    packet: CompletionPacket,

    /// The pipe that we are writing to.
    pipe: Writer,

    /// We logically own the writer, but we don't actually use it.
    _reader: PhantomData<W>,
}

impl<W: Write + Send + 'static> UnblockedWriter<W> {
    /// Spawn a new unblocked writer.
    pub fn new(mut sink: W, poller: &Arc<Poller>, event: Event, pipe_capacity: usize) -> Self {
        // Create a new pipe.
        let (mut reader, writer) = pipe(pipe_capacity);
        let packet = CompletionPacketButSend(CompletionPacket::new(event));

        // Spawn the writer thread.
        let handle = thread::Builder::new()
            .name("alacritty-tty-writer-thread".into())
            .spawn({
                let poller = poller.clone();
                let packet2 = packet.clone();
                move || {
                    let thread = thread::current();
                    let waker = Waker::from(Arc::new(ThreadWaker(thread.clone())));
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
                                packet2.clone().post(&poller);

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

        Self { thread: handle, packet: packet.0, pipe: writer, _reader: PhantomData }
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

/// Forgot to mark it as send.
#[derive(Clone)]
struct CompletionPacketButSend(CompletionPacket);

impl CompletionPacketButSend {
    fn post(self, poller: &Poller) {
        if let Err(e) = poller.post(self.0) {
            log::error!("error posting packet: {}", e);
        }
    }
}

unsafe impl Send for CompletionPacketButSend {}
unsafe impl Sync for CompletionPacketButSend {}
