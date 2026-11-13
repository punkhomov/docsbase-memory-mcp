//! TEMPORARY #155 probe branch only. See the experiment protocol for the schema,
//! candidate boundary, watchdogs, allowed claims, and required disposable host.
//! This is native characterization, not a production regression/acceptance test.
#![cfg(windows)]
// The experiment deliberately calls Win32. Every unsafe block states ownership.
#![allow(unsafe_code)]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("the reviewed #155 probe requires Windows x64");

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NOT_FOUND,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FlushFileBuffers,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelSynchronousIo, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateThread, GetThreadIOPendingFlag, INFINITE, LPTHREAD_START_ROUTINE,
    OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, SwitchToThread, TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject,
};

const FRAME: &[u8; 14] = b"{\"probe\":155}\n";
const SCHEMA: u32 = 1;
const SETUP_MS: u32 = 5_000;
const OBSERVATION_MS: u64 = 5_000;
const ACTION_MS: u64 = 5_000;
const CASE_MS: u64 = 20_000;
const CLEANUP_MS: u64 = 5_000;
const REAP_MS: u64 = 5_000;
const SUITE_MS: u64 = 125_000;
const QUERY_CAP: u64 = 100_000;
const SLOT_COUNT: usize = 64;
const READ_CHUNK_BASE: usize = 40;
const CLAIMED: u32 = u32::MAX;
const PIPE_FLAGS: u32 = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE;
const PIPE_MODE: u32 = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum Case {
    NormalDrain = 1,
    CancelBeforeEntry = 2,
    CancelAfterPositivePending = 3,
    DisconnectAfterPositivePending = 4,
}
impl Case {
    const ALL: [Self; 4] = [
        Self::NormalDrain,
        Self::CancelBeforeEntry,
        Self::CancelAfterPositivePending,
        Self::DisconnectAfterPositivePending,
    ];
    fn parse(value: &str) -> io::Result<Self> {
        Self::ALL
            .into_iter()
            .find(|case| format!("{case:?}") == value)
            .ok_or_else(|| io::Error::other("invalid child case"))
    }
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum Event {
    ChildStarted,
    ConnectSubmitted,
    ClientOpened,
    ConnectCompleted,
    WriteSubmitted,
    WriteCompleted,
    SetupComplete,
    WorkerCreated,
    ClientCreated,
    AtEntryGate,
    EntryWait,
    ControlQuery,
    AbortThreadCreated,
    AbortBegin,
    AbortReturned,
    AbortSignaled,
    EntryReleased,
    QueryBegin,
    QuerySummary,
    PendingObserved,
    ReadGateReleased,
    ReadStarted,
    ReadComplete,
    FlushReturned,
    WorkerExitReleased,
    WorkerExitWait,
    WorkerSignaled,
    CandidateReady,
    CleanupObserved,
    NormalDisconnect,
    ClientEndRead,
    ClientClose,
    ClientSignaled,
    CleanupComplete,
    ChildError,
    WorkerFault,
    ClientFault,
    ConnectWait,
    WriteWait,
    ClientGateWait,
    CleanupError = 54,
    WorkerGateWait,
    CleanupAbortWait,
    CleanupGateWait,
}
const EVENT_NAMES: [&str; 40] = [
    "ChildStarted",
    "ConnectSubmitted",
    "ClientOpened",
    "ConnectCompleted",
    "WriteSubmitted",
    "WriteCompleted",
    "SetupComplete",
    "WorkerCreated",
    "ClientCreated",
    "AtEntryGate",
    "EntryWait",
    "ControlQuery",
    "AbortThreadCreated",
    "AbortBegin",
    "AbortReturned",
    "AbortSignaled",
    "EntryReleased",
    "QueryBegin",
    "QuerySummary",
    "PendingObserved",
    "ReadGateReleased",
    "ReadStarted",
    "ReadComplete",
    "FlushReturned",
    "WorkerExitReleased",
    "WorkerExitWait",
    "WorkerSignaled",
    "CandidateReady",
    "CleanupObserved",
    "NormalDisconnect",
    "ClientEndRead",
    "ClientClose",
    "ClientSignaled",
    "CleanupComplete",
    "ChildError",
    "WorkerFault",
    "ClientFault",
    "ConnectWait",
    "WriteWait",
    "ClientGateWait",
];

// One writer claims each slot, fills atomic fields, then publishes its sequence.
// No reader accesses an unpublished slot's payload. Windows x64 lock-free atomic
// instructions provide interlocked publication in the shared mapped pages. All
// payload accesses are atomic too, including supervisor snapshots during a hang.
#[repr(C, align(8))]
struct Slot {
    sequence: AtomicU32,
    cleanup: AtomicU32,
    args: [AtomicU64; 8],
}
impl Slot {
    fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            cleanup: AtomicU32::new(0),
            args: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}
#[repr(C, align(8))]
struct Records {
    schema: AtomicU32,
    next_sequence: AtomicU32,
    publication_lock: AtomicU32,
    cleanup: AtomicU32,
    read_released: AtomicU32,
    read_started: AtomicU32,
    duplicate_publication: AtomicU32,
    error_text: [[AtomicU32; 256]; 2],
    slots: [Slot; SLOT_COUNT],
}
impl Records {
    fn new() -> Self {
        Self {
            schema: AtomicU32::new(SCHEMA),
            next_sequence: AtomicU32::new(0),
            publication_lock: AtomicU32::new(0),
            cleanup: AtomicU32::new(0),
            read_released: AtomicU32::new(0),
            read_started: AtomicU32::new(0),
            duplicate_publication: AtomicU32::new(0),
            error_text: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            slots: std::array::from_fn(|_| Slot::new()),
        }
    }
    fn publish(&self, event: Event, args: [u64; 8]) {
        self.publish_index(event as usize, args);
    }
    fn publish_index(&self, index: usize, args: [u64; 8]) {
        let slot = &self.slots[index];
        if slot
            .sequence
            .compare_exchange(0, CLAIMED, SeqCst, SeqCst)
            .is_err()
        {
            self.duplicate_publication.store(1, SeqCst);
            return;
        }
        // Serialize only record publication, never a native call. If a writer
        // stalls here, the supervisor still reads already published slots and
        // freezes missing evidence as failure. It never takes this child lock.
        while self
            .publication_lock
            .compare_exchange(0, 1, SeqCst, SeqCst)
            .is_err()
        {
            std::hint::spin_loop();
        }
        // Diagnostic sample only: supervisor classifies late commits against
        // its immutable frozen snapshot, never against this pre-commit load.
        slot.cleanup.store(self.cleanup.load(SeqCst), SeqCst);
        for (field, value) in slot.args.iter().zip(args) {
            field.store(value, SeqCst);
        }
        let sequence = self.next_sequence.fetch_add(1, SeqCst) + 1;
        slot.sequence.store(sequence, SeqCst);
        self.publication_lock.store(0, SeqCst);
    }
    fn record_error(&self, index: usize, event: Event, error: &io::Error) {
        let text = error.to_string();
        let count = text.len().min(255);
        for (field, byte) in self.error_text[index].iter().zip(text.bytes().take(count)) {
            field.store(u32::from(byte), SeqCst);
        }
        self.publish(
            event,
            [
                error.raw_os_error().unwrap_or(0) as u32 as u64,
                count as u64,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
    }
    fn error_message(&self, index: usize) -> String {
        let bytes: Vec<u8> = self.error_text[index]
            .iter()
            .map(|field| field.load(SeqCst) as u8)
            .take_while(|byte| *byte != 0)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
    fn get(&self, event: Event) -> Option<[u64; 8]> {
        self.get_index(event as usize).map(|(_, _, args)| args)
    }
    fn get_index(&self, index: usize) -> Option<(u32, u32, [u64; 8])> {
        let slot = &self.slots[index];
        let sequence = slot.sequence.load(SeqCst);
        if sequence == 0 || sequence == CLAIMED {
            return None;
        }
        Some((
            sequence,
            slot.cleanup.load(SeqCst),
            std::array::from_fn(|i| slot.args[i].load(SeqCst)),
        ))
    }
}

struct Mapping {
    _handle: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}
impl Mapping {
    fn create(name: &[u16]) -> io::Result<Self> {
        assert!(size_of::<Records>() <= 65_536);
        // SAFETY: page-file mapping, null security, valid NUL-terminated name;
        // the checked nonzero size fits u32. Returned handle becomes uniquely owned.
        let raw = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                null(),
                PAGE_READWRITE,
                0,
                u32::try_from(size_of::<Records>()).unwrap(),
                name.as_ptr(),
            )
        };
        // CreateFileMapping documents last-error even on successful existing open.
        // SAFETY: GetLastError is thread-local and captured before another API.
        let error = unsafe { GetLastError() };
        let handle = own(raw)?;
        if error == ERROR_ALREADY_EXISTS {
            return Err(io::Error::other("mapping name collision; no retry"));
        }
        let mapping = Self::map(handle)?;
        // SAFETY: fresh page-aligned writable mapping of exactly Records size;
        // no child has been launched and all atomics are initialized before sharing.
        unsafe { mapping.view.Value.cast::<Records>().write(Records::new()) };
        Ok(mapping)
    }
    fn open(name: &[u16]) -> io::Result<Self> {
        // SAFETY: valid name; no inheritance; returned handle becomes uniquely owned.
        let handle = own(unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name.as_ptr()) })?;
        let mapping = Self::map(handle)?;
        if mapping.records().schema.load(SeqCst) != SCHEMA {
            return Err(io::Error::other("shared schema mismatch"));
        }
        Ok(mapping)
    }
    fn map(handle: OwnedHandle) -> io::Result<Self> {
        // SAFETY: live mapping handle; offset zero and exact initialized record size.
        let view = unsafe {
            MapViewOfFile(
                raw(&handle),
                FILE_MAP_ALL_ACCESS,
                0,
                0,
                size_of::<Records>(),
            )
        };
        if view.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _handle: handle,
            view,
        })
    }
    fn records(&self) -> &Records {
        // SAFETY: the view is aligned, initialized by the supervisor before spawn,
        // lives as long as self, and all shared accesses use lock-free atomics.
        unsafe { &*self.view.Value.cast::<Records>() }
    }
    fn unmap(self) -> io::Result<()> {
        // SAFETY: only the supervisor calls this after it stops reading; each child
        // has its own view. Child views are intentionally retained until exit.
        let result = unsafe { UnmapViewOfFile(self.view) };
        check(result)
    }
}
// No Drop for the view: a child retains it through process teardown, including
// error paths with live actors. Supervisor explicitly unmaps its independent view.

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle().cast()
}
fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: caller passes a newly created/opened valid handle with unique ownership.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
}
fn status(result: i32) -> (u64, u64) {
    // SAFETY: capture thread-local last error immediately, only on BOOL failure.
    let error = if result == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    (result as u32 as u64, u64::from(error))
}
fn check(result: i32) -> io::Result<()> {
    let (ok, error) = status(result);
    if ok == 0 {
        Err(io::Error::from_raw_os_error(error as i32))
    } else {
        Ok(())
    }
}
fn event(name: Option<&[u16]>) -> io::Result<OwnedHandle> {
    // SAFETY: default security, manual-reset, initially unsignaled, optional valid name.
    own(unsafe { CreateEventW(null(), 1, 0, name.map_or(null(), |name| name.as_ptr())) })
}
fn signal(handle: HANDLE) -> io::Result<()> {
    // SAFETY: handle is a retained event, never closed while a user is active.
    check(unsafe { SetEvent(handle) })
}
fn wait(handle: HANDLE, milliseconds: u32) -> (u32, u32) {
    // SAFETY: retained waitable handle; supervisor/controller timeouts are finite.
    let result = unsafe { WaitForSingleObject(handle, milliseconds) };
    // SAFETY: capture immediately only for WAIT_FAILED.
    let error = if result == WAIT_FAILED {
        unsafe { GetLastError() }
    } else {
        0
    };
    (result, error)
}
fn start(entry: LPTHREAD_START_ROUTINE, context: *const c_void) -> io::Result<(OwnedHandle, u32)> {
    let mut id = 0;
    // SAFETY: the raw entry is a non-unwinding Win32 routine. Its boxed context,
    // handles and mapping are retained until thread signaling or child teardown.
    // Null security creates a real, non-inheritable full-access thread handle,
    // including QUERY_INFORMATION, TERMINATE and SYNCHRONIZE rights.
    let thread = own(unsafe { CreateThread(null(), 0, entry, context, 0, &mut id) })?;
    Ok((thread, id))
}

struct WorkerContext {
    records: *const Records,
    server: HANDLE,
    at_entry: HANDLE,
    entry: HANDLE,
    exit: HANDLE,
}
unsafe extern "system" fn flush_worker(pointer: *mut c_void) -> u32 {
    // SAFETY: unique boxed immutable context, records and all handles retained by
    // the child until the real thread handle signals; never a pooled thread.
    let context = unsafe { &*pointer.cast::<WorkerContext>() };
    // SAFETY: the mapping has the same lifetime and only atomic fields are accessed.
    let records = unsafe { &*context.records };
    records.publish(Event::AtEntryGate, [0; 8]);
    // SAFETY: retained valid event handle. This routine performs no logging, CRT,
    // setup/read/write or teardown I/O; only one FlushFileBuffers is submitted.
    let (ok, error) = status(unsafe { SetEvent(context.at_entry) });
    if ok == 0 {
        records.publish(Event::WorkerFault, [1, error, 0, 0, 0, 0, 0, 0]);
        return 1;
    }
    let (result, error) = wait(context.entry, INFINITE);
    records.publish(
        Event::EntryWait,
        [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
    );
    if result != WAIT_OBJECT_0 {
        return 2;
    }
    // SAFETY: retained writable server endpoint with fully completed setup I/O.
    // No endpoint close or context retirement races this one synchronous call.
    let (ok, error) = status(unsafe { FlushFileBuffers(context.server) });
    records.publish(Event::FlushReturned, [ok, error, 0, 0, 0, 0, 0, 0]);
    // The worker remains reserved after terminal publication. The controller
    // releases this event only after all query/abort activity is finished.
    let (result, error) = wait(context.exit, INFINITE);
    records.publish(
        Event::WorkerExitWait,
        [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
    );
    u32::from(result != WAIT_OBJECT_0)
}

struct AbortContext {
    records: *const Records,
    target: HANDLE,
    disconnect: bool,
}
unsafe extern "system" fn abort_actor(pointer: *mut c_void) -> u32 {
    // SAFETY: immutable boxed context and target handle retained through signaling.
    let context = unsafe { &*pointer.cast::<AbortContext>() };
    // SAFETY: shared mapping remains alive; publications only access atomics.
    let records = unsafe { &*context.records };
    records.publish(
        Event::AbortBegin,
        [u64::from(context.disconnect), 0, 0, 0, 0, 0, 0, 0],
    );
    // SAFETY: exactly one call on the retained owned worker (with TERMINATE right)
    // or the retained isolated server endpoint. Never target a remembered pool ID.
    let (ok, error) = status(unsafe {
        if context.disconnect {
            DisconnectNamedPipe(context.target)
        } else {
            CancelSynchronousIo(context.target)
        }
    });
    records.publish(
        Event::AbortReturned,
        [ok, error, u64::from(context.disconnect), 0, 0, 0, 0, 0],
    );
    0
}

struct ClientContext {
    records: *const Records,
    pipe: Option<OwnedHandle>,
    read_gate: HANDLE,
    cleanup: HANDLE,
    case: Case,
}
unsafe extern "system" fn client_actor(pointer: *mut c_void) -> u32 {
    // SAFETY: this actor exclusively owns the mutable context/endpoint. Controller
    // never accesses it again; the allocation is retained until process teardown.
    let context = unsafe { &mut *pointer.cast::<ClientContext>() };
    // SAFETY: mapped atomic storage remains alive until child teardown.
    let records = unsafe { &*context.records };
    let handles = [context.cleanup, context.read_gate];
    // SAFETY: both event handles and the array are valid for the whole wait.
    let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    // SAFETY: immediate thread-local error on failed wait.
    let error = if result == WAIT_FAILED {
        unsafe { GetLastError() }
    } else {
        0
    };
    records.publish(
        Event::ClientGateWait,
        [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
    );
    if result != WAIT_OBJECT_0 && result != WAIT_OBJECT_0 + 1 {
        records.publish(
            Event::ClientFault,
            [1, u64::from(result), u64::from(error), 0, 0, 0, 0, 0],
        );
        return 1;
    }
    let pipe = raw(context.pipe.as_ref().unwrap());
    let drain = context.case == Case::NormalDrain || context.case == Case::CancelBeforeEntry;
    if drain {
        // Normal reads require the explicit read gate. Pre-entry reads are cleanup
        // only, after the supervisor has frozen the escaped-operation observation.
        if context.case == Case::NormalDrain && records.read_released.load(SeqCst) == 0 {
            close_client(context, records);
            return 2;
        }
        records.read_started.fetch_add(1, SeqCst);
        records.publish(Event::ReadStarted, [0; 8]);
        let mut frame = [0_u8; 14];
        let mut total = 0_usize;
        for index in 0..FRAME.len() {
            let mut transferred = 0;
            // SAFETY: sync client handle is exclusively owned by this actor;
            // remaining buffer and count stay live through this blocking call.
            // No other actor closes the endpoint, even if this read hangs.
            let (ok, error) = status(unsafe {
                ReadFile(
                    pipe,
                    frame[total..].as_mut_ptr(),
                    (FRAME.len() - total) as u32,
                    &mut transferred,
                    null_mut(),
                )
            });
            if transferred as usize > FRAME.len() - total {
                records.publish(
                    Event::ClientFault,
                    [2, u64::from(transferred), 0, 0, 0, 0, 0, 0],
                );
                return 3;
            }
            total += transferred as usize;
            records.publish_index(
                READ_CHUNK_BASE + index,
                [ok, error, u64::from(transferred), total as u64, 0, 0, 0, 0],
            );
            if ok == 0 {
                records.publish(Event::ClientFault, [4, error, index as u64, 0, 0, 0, 0, 0]);
            }
            if ok == 0 || transferred == 0 || total == FRAME.len() {
                break;
            }
        }
        let mut tail = [0_u8; 8];
        tail[..6].copy_from_slice(&frame[8..]);
        records.publish(
            Event::ReadComplete,
            [
                total as u64,
                u64::from(frame == *FRAME),
                u64::from_le_bytes(frame[..8].try_into().unwrap()),
                u64::from_le_bytes(tail),
                0,
                0,
                0,
                0,
            ],
        );
    }
    // The owner holds its endpoint open after reading. Only the supervisor may
    // release cleanup, and it writes CandidateFrozen to disk/stdout first.
    let (result, error) = wait(context.cleanup, INFINITE);
    if result != WAIT_OBJECT_0 {
        records.publish(
            Event::ClientFault,
            [3, u64::from(result), u64::from(error), 0, 0, 0, 0, 0],
        );
        return 4;
    }
    if drain || context.case == Case::DisconnectAfterPositivePending {
        let mut byte = 0;
        let mut transferred = 0;
        // SAFETY: owned sync endpoint/buffer, only this actor reads/closes it.
        // A blocked end-read stays owned and is contained by the process/VM.
        let (ok, error) =
            status(unsafe { ReadFile(pipe, &mut byte, 1, &mut transferred, null_mut()) });
        records.publish(
            Event::ClientEndRead,
            [
                ok,
                error,
                u64::from(transferred),
                u64::from(byte),
                0,
                0,
                0,
                0,
            ],
        );
    }
    close_client(context, records);
    0
}
fn close_client(context: &mut ClientContext, records: &Records) {
    // Transfer unique ownership to this explicit close, recording its raw status.
    let handle = context.pipe.take().unwrap().into_raw_handle();
    // SAFETY: this actor owns the only client handle and all its reads returned.
    let (ok, error) = status(unsafe { CloseHandle(handle.cast()) });
    records.publish(Event::ClientClose, [ok, error, 0, 0, 0, 0, 0, 0]);
}

struct Fixture {
    server: Option<OwnedHandle>,
    client: Option<OwnedHandle>,
    connect_event: OwnedHandle,
    write_event: OwnedHandle,
    connect: OVERLAPPED,
    write: OVERLAPPED,
    at_entry: OwnedHandle,
    entry: OwnedHandle,
    worker_exit: OwnedHandle,
    read_gate: OwnedHandle,
    worker: Option<OwnedHandle>,
    client_thread: Option<OwnedHandle>,
    abort: Option<OwnedHandle>,
}
impl Fixture {
    fn allocate() -> io::Result<Box<Self>> {
        let connect_event = event(None)?;
        let write_event = event(None)?;
        let mut connect = OVERLAPPED::default();
        connect.hEvent = raw(&connect_event);
        let mut write = OVERLAPPED::default();
        write.hEvent = raw(&write_event);
        Ok(Box::new(Self {
            server: None,
            client: None,
            connect_event,
            write_event,
            connect,
            write,
            at_entry: event(None)?,
            entry: event(None)?,
            worker_exit: event(None)?,
            read_gate: event(None)?,
            worker: None,
            client_thread: None,
            abort: None,
        }))
    }
    fn server(&self) -> HANDLE {
        raw(self.server.as_ref().unwrap())
    }
    fn setup(&mut self, records: &Records, name: &[u16]) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(SETUP_MS));
        // SAFETY: valid NUL-terminated unique local name, one instance, byte/wait
        // mode, writable OVERLAPPED server; default security, no IOCP association.
        self.server = Some(own(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_FLAGS,
                PIPE_MODE,
                1,
                4096,
                4096,
                0,
                null(),
            )
        })?);
        // SAFETY: boxed OVERLAPPED/event and server remain stable and retained even
        // on timeout. Exactly one connect submission, no close/cancel fallback.
        let (ok, error) = status(unsafe { ConnectNamedPipe(self.server(), &mut self.connect) });
        records.publish(Event::ConnectSubmitted, [ok, error, 0, 0, 0, 0, 0, 0]);
        if ok != 0 || error != u64::from(ERROR_IO_PENDING) {
            return Err(io::Error::other(
                "connect did not enter expected isolated pending setup",
            ));
        }
        // SAFETY: same pipe, synchronous client endpoint, default security and no
        // sharing/inheritance. This endpoint is not read before the explicit gate.
        let client = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        let error = if client == INVALID_HANDLE_VALUE {
            // SAFETY: capture immediately on failed CreateFileW.
            unsafe { GetLastError() }
        } else {
            0
        };
        records.publish(
            Event::ClientOpened,
            [
                u64::from(client != INVALID_HANDLE_VALUE),
                u64::from(error),
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        self.client = Some(own(client)?);
        let (result, error) = wait(raw(&self.connect_event), remaining_ms(deadline)?);
        records.publish(
            Event::ConnectWait,
            [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
        );
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::other("connect wait not signaled"));
        }
        let mut bytes = 0;
        // SAFETY: completed connect event and retained OVERLAPPED/endpoint; no wait.
        let (ok, error) =
            status(unsafe { GetOverlappedResult(self.server(), &self.connect, &mut bytes, 0) });
        records.publish(
            Event::ConnectCompleted,
            [ok, error, u64::from(bytes), 0, 0, 0, 0, 0],
        );
        if ok == 0 {
            return Err(io::Error::other("connect terminal failure"));
        }
        let mut immediate = 0;
        // SAFETY: static 14-byte frame and boxed OVERLAPPED/event remain valid
        // through terminal completion, including setup failure/child termination.
        let (ok, error) = status(unsafe {
            WriteFile(
                self.server(),
                FRAME.as_ptr(),
                FRAME.len() as u32,
                &mut immediate,
                &mut self.write,
            )
        });
        records.publish(
            Event::WriteSubmitted,
            [ok, error, u64::from(immediate), 0, 0, 0, 0, 0],
        );
        if ok == 0 && error != u64::from(ERROR_IO_PENDING) {
            return Err(io::Error::other("write submission failure"));
        }
        if ok == 0 {
            let (result, error) = wait(raw(&self.write_event), remaining_ms(deadline)?);
            records.publish(
                Event::WriteWait,
                [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
            );
            if result != WAIT_OBJECT_0 {
                return Err(io::Error::other("write wait not signaled"));
            }
        }
        // SAFETY: immediate completion or signaled event; retained stable setup
        // request and frame. GetOverlappedResult is nonblocking and called once.
        let (ok, error) =
            status(unsafe { GetOverlappedResult(self.server(), &self.write, &mut bytes, 0) });
        records.publish(
            Event::WriteCompleted,
            [ok, error, u64::from(bytes), 0, 0, 0, 0, 0],
        );
        if ok == 0 || bytes as usize != FRAME.len() {
            return Err(io::Error::other(
                "setup did not terminally write exactly 14 bytes",
            ));
        }
        records.publish(
            Event::SetupComplete,
            [
                u64::from(PIPE_FLAGS),
                u64::from(PIPE_MODE),
                4096,
                FRAME.len() as u64,
                0,
                0,
                0,
                0,
            ],
        );
        Ok(())
    }
    fn actors(&mut self, records: &Records, cleanup: HANDLE, case: Case) -> io::Result<()> {
        let client_context = Box::into_raw(Box::new(ClientContext {
            records,
            pipe: self.client.take(),
            read_gate: raw(&self.read_gate),
            cleanup,
            case,
        }));
        let (thread, id) = start(Some(client_actor), client_context.cast())?;
        records.publish(Event::ClientCreated, [u64::from(id), 0, 0, 0, 0, 0, 0, 0]);
        self.client_thread = Some(thread);
        let worker_context = Box::into_raw(Box::new(WorkerContext {
            records,
            server: self.server(),
            at_entry: raw(&self.at_entry),
            entry: raw(&self.entry),
            exit: raw(&self.worker_exit),
        }));
        let (thread, id) = start(Some(flush_worker), worker_context.cast())?;
        records.publish(
            Event::WorkerCreated,
            [
                u64::from(id),
                thread.as_raw_handle() as usize as u64,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        self.worker = Some(thread);
        let (result, error) = wait(raw(&self.at_entry), SETUP_MS);
        records.publish(
            Event::WorkerGateWait,
            [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
        );
        if result == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(io::Error::other("worker entry gate not signaled"))
        }
    }
    fn abort(&mut self, records: &Records, disconnect: bool) -> io::Result<()> {
        if self.abort.is_some() {
            return Err(io::Error::other("extra abort forbidden"));
        }
        let context = Box::into_raw(Box::new(AbortContext {
            records,
            target: if disconnect {
                self.server()
            } else {
                raw(self.worker.as_ref().unwrap())
            },
            disconnect,
        }));
        let (thread, id) = start(Some(abort_actor), context.cast())?;
        records.publish(
            Event::AbortThreadCreated,
            [u64::from(id), 0, 0, 0, 0, 0, 0, 0],
        );
        self.abort = Some(thread);
        Ok(())
    }
    fn abort_finished(&self, records: &Records, deadline: Instant) -> io::Result<[u64; 8]> {
        wait_records(records, &[Event::AbortReturned], deadline)?;
        let (result, error) = wait(raw(self.abort.as_ref().unwrap()), remaining_ms(deadline)?);
        records.publish(
            Event::AbortSignaled,
            [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
        );
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::other("abort actor not signaled"));
        }
        Ok(records.get(Event::AbortReturned).unwrap())
    }
    fn retire_worker(&self, records: &Records, deadline: Instant) -> io::Result<()> {
        // All queries are finished and the abort actor has actually terminated.
        signal(raw(&self.worker_exit))?;
        records.publish(Event::WorkerExitReleased, [0; 8]);
        let (result, error) = wait(raw(self.worker.as_ref().unwrap()), remaining_ms(deadline)?);
        records.publish(
            Event::WorkerSignaled,
            [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
        );
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::other("worker not signaled"));
        }
        if records
            .get(Event::WorkerExitWait)
            .is_none_or(|w| w[0] != u64::from(WAIT_OBJECT_0))
            || records.get(Event::WorkerFault).is_some()
        {
            return Err(io::Error::other(
                "worker termination had a native synchronization failure",
            ));
        }
        Ok(())
    }
}
fn remaining_ms(deadline: Instant) -> io::Result<u32> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::other("deadline exhausted"))?;
    Ok(remaining.as_millis().max(1).min(u128::from(u32::MAX - 1)) as u32)
}
fn wait_records(records: &Records, events: &[Event], deadline: Instant) -> io::Result<()> {
    loop {
        if events.iter().all(|event| records.get(*event).is_some()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("terminal publication deadline"));
        }
        // SAFETY: scheduling yield, no I/O, sleep or submission/retry of an operation.
        unsafe { SwitchToThread() };
    }
}
fn observe_pending(records: &Records, worker: HANDLE) -> io::Result<()> {
    let started = Instant::now();
    records.publish(
        Event::QueryBegin,
        [QUERY_CAP, OBSERVATION_MS, 0, 0, 0, 0, 0, 0],
    );
    for count in 1..=QUERY_CAP {
        if started.elapsed().as_millis() >= u128::from(OBSERVATION_MS)
            || records.get(Event::FlushReturned).is_some()
        {
            records.publish(
                Event::QuerySummary,
                [
                    count - 1,
                    count - 1,
                    0,
                    0,
                    0,
                    0,
                    started.elapsed().as_nanos() as u64,
                    1,
                ],
            );
            return Err(io::Error::other("NoPositivePendingObservation"));
        }
        let begin = started.elapsed().as_nanos() as u64;
        let mut pending = 0;
        // SAFETY: retained full-access owned single-purpose worker; output BOOL is
        // separate from the API return. Querying does not resubmit the flush.
        let (ok, error) = status(unsafe { GetThreadIOPendingFlag(worker, &mut pending) });
        let end = started.elapsed().as_nanos() as u64;
        if ok == 0 || pending != 0 {
            records.publish(
                Event::QuerySummary,
                [
                    count,
                    count - 1,
                    ok,
                    pending as u32 as u64,
                    error,
                    begin,
                    end,
                    0,
                ],
            );
            if ok == 0 {
                return Err(io::Error::other("pending query failed"));
            }
            let released = records.read_released.load(SeqCst);
            let reads = records.read_started.load(SeqCst);
            records.publish(
                Event::PendingObserved,
                [
                    count,
                    begin,
                    end,
                    ok,
                    pending as u32 as u64,
                    u64::from(released),
                    u64::from(reads),
                    0,
                ],
            );
            if released != 0 || reads != 0 {
                return Err(io::Error::other("read gate violated before TRUE sample"));
            }
            return Ok(());
        }
        // SAFETY: bounded read-only observation of the same operation; no sleeps,
        // reissued flush, cancellation retry or timing-based pending inference.
        unsafe { SwitchToThread() };
    }
    records.publish(
        Event::QuerySummary,
        [
            QUERY_CAP,
            QUERY_CAP,
            1,
            0,
            0,
            0,
            started.elapsed().as_nanos() as u64,
            2,
        ],
    );
    Err(io::Error::other("NoPositivePendingObservation: query cap"))
}

// CandidateReady codes: 1 normal controls, 2 escaped pre-entry operation,
// 3 observed aborted flush, 4 disconnect plus terminal flush. 100 is failure.
fn candidate(fixture: &mut Fixture, records: &Records, case: Case) -> io::Result<u64> {
    let worker = raw(fixture.worker.as_ref().unwrap());
    let mut pending = 0;
    // SAFETY: real retained worker at the entry gate, query-information right.
    let (ok, error) = status(unsafe { GetThreadIOPendingFlag(worker, &mut pending) });
    records.publish(
        Event::ControlQuery,
        [ok, pending as u32 as u64, error, 0, 0, 0, 0, 0],
    );
    if ok == 0 || pending != 0 {
        return Err(io::Error::other("entry control must be successful FALSE"));
    }
    if case == Case::CancelBeforeEntry {
        let deadline = Instant::now() + Duration::from_millis(ACTION_MS);
        fixture.abort(records, false)?;
        let abort = fixture.abort_finished(records, deadline)?;
        if abort[0] != 0 || abort[1] != u64::from(ERROR_NOT_FOUND) {
            return Err(io::Error::other(
                "pre-entry cancel control must be FALSE/NOT_FOUND",
            ));
        }
    }
    records.publish(Event::EntryReleased, [0; 8]);
    signal(raw(&fixture.entry))?;
    observe_pending(records, worker)?;
    if case == Case::CancelBeforeEntry {
        return Ok(2);
    }
    let deadline = Instant::now() + Duration::from_millis(ACTION_MS);
    if case == Case::NormalDrain {
        records.read_released.store(1, SeqCst);
        records.publish(Event::ReadGateReleased, [0; 8]);
        signal(raw(&fixture.read_gate))?;
        wait_records(
            records,
            &[Event::ReadComplete, Event::FlushReturned],
            deadline,
        )?;
        let read = records.get(Event::ReadComplete).unwrap();
        let flush = records.get(Event::FlushReturned).unwrap();
        if read[0] != FRAME.len() as u64
            || read[1] != 1
            || flush[0] == 0
            || records.get(Event::ClientFault).is_some()
        {
            return Err(io::Error::other("normal delivery bytes/flush failed"));
        }
        fixture.abort(records, false)?;
        let abort = fixture.abort_finished(records, deadline)?;
        if abort[0] != 0 || abort[1] != u64::from(ERROR_NOT_FOUND) {
            return Err(io::Error::other(
                "post-terminal cancel control must be FALSE/NOT_FOUND",
            ));
        }
        fixture.retire_worker(records, deadline)?;
        return Ok(1);
    }
    let disconnect = case == Case::DisconnectAfterPositivePending;
    fixture.abort(records, disconnect)?;
    wait_records(
        records,
        &[Event::AbortReturned, Event::FlushReturned],
        deadline,
    )?;
    let abort = fixture.abort_finished(records, deadline)?;
    fixture.retire_worker(records, deadline)?;
    let flush = records.get(Event::FlushReturned).unwrap();
    if disconnect {
        if abort[0] == 0 {
            return Err(io::Error::other(
                "disconnect returned failure; raw result retained",
            ));
        }
        return Ok(4); // Any terminal flush status retained; never claim full delivery.
    }
    if abort[0] == 0 && abort[1] != u64::from(ERROR_NOT_FOUND) {
        return Err(io::Error::other("cancel returned unexpected error"));
    }
    if flush[0] == 0 && flush[1] == u64::from(ERROR_OPERATION_ABORTED) {
        return Ok(3);
    }
    Err(io::Error::other(
        "flush was not observed aborted; exact terminal status retained",
    ))
}

fn cleanup(fixture: &Fixture, records: &Records, case: Case) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(CLEANUP_MS);
    records.publish(Event::CleanupObserved, [0; 8]);
    // If an abort or query failed, do not release the worker while an abort is
    // active. Endpoint cleanup is exclusively the client's independent actor.
    if let Some(abort) = &fixture.abort {
        let (result, error) = wait(raw(abort), remaining_ms(deadline)?);
        records.publish(
            Event::CleanupAbortWait,
            [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
        );
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::other("cleanup abort actor not signaled"));
        }
    }
    if fixture.worker.is_some()
        && records.get(Event::FlushReturned).is_some()
        && records.get(Event::WorkerSignaled).is_none()
    {
        fixture.retire_worker(records, deadline)?;
    }
    if case == Case::NormalDrain || case == Case::CancelBeforeEntry {
        wait_records(
            records,
            &[Event::ReadComplete, Event::FlushReturned],
            deadline,
        )?;
        let read = records.get(Event::ReadComplete).unwrap();
        let flush = records.get(Event::FlushReturned).unwrap();
        if read[0] != FRAME.len() as u64
            || read[1] != 1
            || flush[0] == 0
            || records.get(Event::ClientFault).is_some()
        {
            return Err(io::Error::other("cleanup drain bytes/flush failed"));
        }
        if records.get(Event::WorkerSignaled).is_none() {
            fixture.retire_worker(records, deadline)?;
        }
        // SAFETY: setup is terminal; worker and abort are signaled; full frame was
        // read and successful flush observed. No concurrent server user remains.
        let (ok, error) = status(unsafe { DisconnectNamedPipe(fixture.server()) });
        records.publish(Event::NormalDisconnect, [ok, error, 0, 0, 0, 0, 0, 0]);
        if ok == 0 {
            return Err(io::Error::other("normal disconnect failed"));
        }
    } else if fixture.worker.is_some() && records.get(Event::WorkerSignaled).is_none() {
        wait_records(records, &[Event::FlushReturned], deadline)?;
        fixture.retire_worker(records, deadline)?;
    }
    if let Some(client) = &fixture.client_thread {
        let (result, error) = wait(raw(client), remaining_ms(deadline)?);
        records.publish(
            Event::ClientSignaled,
            [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
        );
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::other("client actor not signaled"));
        }
        if records
            .get(Event::ClientClose)
            .is_none_or(|close| close[0] == 0)
            || records.get(Event::ClientFault).is_some()
        {
            return Err(io::Error::other("client close/fault failure"));
        }
        if case == Case::NormalDrain || case == Case::CancelBeforeEntry {
            let end = records
                .get(Event::ClientEndRead)
                .ok_or_else(|| io::Error::other("missing end-read"))?;
            // After full drain and successful normal disconnect, the client
            // read may report a disconnected pipe. Preserve its raw 233 record.
            if !(end[0] == 0 && end[1] == u64::from(ERROR_BROKEN_PIPE)
                || end[0] == 0 && end[1] == u64::from(ERROR_PIPE_NOT_CONNECTED) && end[2] == 0
                || end[0] != 0 && end[2] == 0)
            {
                return Err(io::Error::other("unexpected normal end-read status"));
            }
        }
    }
    records.publish(Event::CleanupComplete, [1, 0, 0, 0, 0, 0, 0, 0]);
    Ok(())
}
fn child(case: Case, name: &str) -> io::Result<()> {
    let mapping = Box::leak(Box::new(Mapping::open(&wide(name))?));
    let records = mapping.records();
    // SAFETY: existing valid named event; child only waits, supervisor signals.
    let cleanup_event = Box::leak(Box::new(own(unsafe {
        OpenEventW(
            SYNCHRONIZATION_SYNCHRONIZE,
            0,
            wide(&format!("{name}-cleanup")).as_ptr(),
        )
    })?));
    records.publish(
        Event::ChildStarted,
        [u64::from(std::process::id()), case as u64, 0, 0, 0, 0, 0, 0],
    );
    // Leak the boxed fixture before the first setup submission. Error paths must
    // never drop an endpoint/event/OVERLAPPED still used by a kernel operation.
    // All child allocations/handles are finally owned by process teardown, after
    // CandidateFrozen. This fixed one-child leak is deliberate probe containment.
    let fixture = Box::leak(Fixture::allocate()?);
    let result = fixture
        .setup(
            records,
            &wide(&format!(
                r"\\.\pipe\docsbase-155-{}-{}",
                std::process::id(),
                case as u32
            )),
        )
        .and_then(|()| fixture.actors(records, raw(cleanup_event), case))
        .and_then(|()| candidate(fixture, records, case));
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            records.record_error(0, Event::ChildError, &error);
            100
        }
    };
    records.publish(Event::CandidateReady, [code, 0, 0, 0, 0, 0, 0, 0]);
    // Child cannot initiate cleanup or revise candidate evidence. A hung child
    // controller is not required to print evidence: supervisor reads mapped slots.
    let (result, error) = wait(raw(cleanup_event), INFINITE);
    records.publish(
        Event::CleanupGateWait,
        [u64::from(result), u64::from(error), 0, 0, 0, 0, 0, 0],
    );
    let cleaned = if result == WAIT_OBJECT_0 {
        cleanup(fixture, records, case)
    } else {
        Err(io::Error::other("cleanup gate wait failed"))
    };
    if let Err(error) = &cleaned {
        records.record_error(1, Event::CleanupError, error);
    }
    // The mapping, cleanup event and fixture were leaked before actors started.
    // No error path drops anything that a native actor might still reference.
    cleaned?;
    if code >= 100 {
        return Err(io::Error::other("candidate failed"));
    }
    Ok(())
}

struct Evidence {
    file: File,
    case: Case,
}
impl Evidence {
    fn emit(&mut self, kind: &str, payload: Value) -> io::Result<()> {
        let value = json!({"schema":SCHEMA, "case":format!("{:?}", self.case), "operation_id":1,
            "supervisor_pid":std::process::id(), "kind":kind, "data":payload});
        serde_json::to_writer(&mut self.file, &value)?;
        writeln!(self.file)?;
        self.file.flush()?;
        self.file.sync_data()?;
        let stdout = io::stdout();
        let mut output = stdout.lock();
        serde_json::to_writer(&mut output, &value)?;
        writeln!(output)?;
        output.flush()
    }
    fn pump(
        &mut self,
        records: &Records,
        seen: &mut [bool; SLOT_COUNT],
        frozen: Option<&Snapshot>,
    ) -> io::Result<()> {
        let mut published = Vec::with_capacity(SLOT_COUNT);
        for (index, observed) in seen.iter().enumerate() {
            if !observed && let Some((sequence, cleanup, args)) = records.get_index(index) {
                published.push((sequence, index, cleanup, args));
            }
        }
        published.sort_by_key(|record| record.0);
        for (sequence, index, cleanup, args) in published {
            let event = event_name(index);
            // The child's cleanup sample precedes its commit and is diagnostic
            // only. After freeze, exact owned snapshot membership is authoritative:
            // a stalled writer absent there cannot later become Candidate.
            let (phase, phase_basis) = match frozen {
                None => ("Candidate", "BeforeCandidateFreeze"),
                Some(snapshot) if snapshot.contains(index, sequence) => {
                    ("Candidate", "FrozenSnapshot")
                }
                Some(_) => ("Cleanup", "AbsentFromFrozenSnapshot"),
            };
            self.emit(
                "NativeRecord",
                json!({"publication_sequence":sequence, "event":event,
                "phase":phase, "phase_basis":phase_basis, "child_cleanup_sample":cleanup, "args":args,
                    "error_message":if index == Event::ChildError as usize {Some(records.error_message(0))}
                        else if index == Event::CleanupError as usize {Some(records.error_message(1))} else {None}}),
            )?;
            seen[index] = true;
        }
        Ok(())
    }
}
fn event_name(index: usize) -> String {
    if let Some(name) = EVENT_NAMES.get(index) {
        return (*name).to_owned();
    }
    if (READ_CHUNK_BASE..READ_CHUNK_BASE + FRAME.len()).contains(&index) {
        return format!("ReadChunk{}", index - READ_CHUNK_BASE);
    }
    match index {
        54 => "CleanupError",
        55 => "WorkerGateWait",
        56 => "CleanupAbortWait",
        57 => "CleanupGateWait",
        _ => "UnusedSlot",
    }
    .to_owned()
}
#[derive(Clone, Copy)]
struct CapturedSlot {
    sequence: u32,
    child_cleanup_sample: u32,
    args: [u64; 8],
}
// Supervisor-owned evidence, captured once. This is not an atomic native-state
// observation. Validation, reason and serialized CandidateFrozen all use this
// same immutable value; none can pick up a later live record or flag.
struct Snapshot {
    slots: [Option<CapturedSlot>; SLOT_COUNT],
    duplicate_publication: u32,
    read_released: u32,
    read_started: u32,
    candidate_error: Option<String>,
    cleanup_error: Option<String>,
    child_exited_before_candidate: bool,
}
impl Snapshot {
    fn capture(records: &Records, child_exited_before_candidate: bool) -> Self {
        let slots: [Option<CapturedSlot>; SLOT_COUNT] = std::array::from_fn(|index| {
            records
                .get_index(index)
                .map(|(sequence, child_cleanup_sample, args)| CapturedSlot {
                    sequence,
                    child_cleanup_sample,
                    args,
                })
        });
        Self {
            duplicate_publication: records.duplicate_publication.load(SeqCst),
            read_released: records.read_released.load(SeqCst),
            read_started: records.read_started.load(SeqCst),
            candidate_error: slots[Event::ChildError as usize]
                .is_some()
                .then(|| records.error_message(0)),
            cleanup_error: slots[Event::CleanupError as usize]
                .is_some()
                .then(|| records.error_message(1)),
            child_exited_before_candidate,
            slots,
        }
    }
    fn get(&self, event: Event) -> Option<[u64; 8]> {
        self.slots[event as usize].map(|slot| slot.args)
    }
    fn get_index(&self, index: usize) -> Option<(u32, u32, [u64; 8])> {
        self.slots[index].map(|slot| (slot.sequence, slot.child_cleanup_sample, slot.args))
    }
    fn contains(&self, index: usize, sequence: u32) -> bool {
        self.slots[index].is_some_and(|slot| slot.sequence == sequence)
    }
    fn to_json(&self) -> Value {
        let slots: Vec<Value> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.map(|slot| {
                    json!({"slot":index, "publication_sequence":slot.sequence,
                "child_cleanup_sample":slot.child_cleanup_sample, "args":slot.args})
                })
            })
            .collect();
        json!({"slots":slots,"duplicate_publication":self.duplicate_publication,
            "read_released":self.read_released,"read_started":self.read_started,
            "candidate_error":self.candidate_error.as_deref(),"cleanup_error":self.cleanup_error.as_deref(),
            "child_exited_before_candidate":self.child_exited_before_candidate})
    }
}
fn validate_candidate(snapshot: &Snapshot, case: Case) -> bool {
    if snapshot.child_exited_before_candidate {
        return false;
    }
    let Some(ready) = snapshot.get(Event::CandidateReady) else {
        return false;
    };
    if ready[0] != case as u64
        || snapshot.duplicate_publication != 0
        || snapshot.get(Event::ChildError).is_some()
        || snapshot.get(Event::WorkerFault).is_some()
        || snapshot.get(Event::ClientFault).is_some()
    {
        return false;
    }
    let Some(pending) = snapshot.get(Event::PendingObserved) else {
        return false;
    };
    if !(1..=QUERY_CAP).contains(&pending[0])
        || pending[3] == 0
        || pending[4] == 0
        || pending[5] != 0
        || pending[6] != 0
    {
        return false;
    }
    if snapshot.get(Event::SetupComplete).is_none_or(|s| {
        s[0] != u64::from(PIPE_FLAGS) || s[1] != u64::from(PIPE_MODE) || s[3] != FRAME.len() as u64
    }) || snapshot
        .get(Event::ConnectCompleted)
        .is_none_or(|c| c[0] == 0)
        || snapshot
            .get(Event::WriteCompleted)
            .is_none_or(|w| w[0] == 0 || w[2] != FRAME.len() as u64)
        || snapshot
            .get(Event::ControlQuery)
            .is_none_or(|q| q[0] == 0 || q[1] != 0)
        || snapshot.get(Event::WorkerCreated).is_none()
        || snapshot.get(Event::ClientCreated).is_none()
        || snapshot
            .get(Event::AbortSignaled)
            .is_none_or(|a| a[0] != u64::from(WAIT_OBJECT_0))
    {
        return false;
    }
    let Some(abort) = snapshot.get(Event::AbortReturned) else {
        return false;
    };
    if case == Case::CancelBeforeEntry {
        return abort[0] == 0
            && abort[1] == u64::from(ERROR_NOT_FOUND)
            && abort[2] == 0
            && snapshot.get(Event::FlushReturned).is_none()
            && snapshot.read_started == 0
            && snapshot.read_released == 0;
    }
    if snapshot
        .get(Event::WorkerSignaled)
        .is_none_or(|w| w[0] != u64::from(WAIT_OBJECT_0))
        || snapshot
            .get(Event::WorkerExitWait)
            .is_none_or(|w| w[0] != u64::from(WAIT_OBJECT_0))
    {
        return false;
    }
    let Some(flush) = snapshot.get(Event::FlushReturned) else {
        return false;
    };
    if case == Case::NormalDrain {
        return flush[0] != 0
            && abort[0] == 0
            && abort[1] == u64::from(ERROR_NOT_FOUND)
            && abort[2] == 0
            && snapshot
                .get(Event::ReadComplete)
                .is_some_and(|r| r[0] == FRAME.len() as u64 && r[1] == 1)
            && (READ_CHUNK_BASE..READ_CHUNK_BASE + FRAME.len())
                .all(|i| snapshot.get_index(i).is_none_or(|(_, _, a)| a[0] != 0))
            && snapshot.read_started == 1
            && snapshot.read_released == 1;
    }
    if snapshot.read_started != 0 || snapshot.read_released != 0 {
        return false;
    }
    if case == Case::DisconnectAfterPositivePending {
        return abort[0] != 0 && abort[2] == 1;
    }
    flush[0] == 0
        && flush[1] == u64::from(ERROR_OPERATION_ABORTED)
        && abort[2] == 0
        && (abort[0] != 0 || abort[1] == u64::from(ERROR_NOT_FOUND))
}
fn run_case(case: Case, directory: &Path, suite_deadline: Instant) -> io::Result<bool> {
    let case_deadline = (Instant::now() + Duration::from_millis(CASE_MS)).min(suite_deadline);
    let name = format!("Local\\docsbase-155-{}-{}", std::process::id(), case as u32);
    let mapping = Mapping::create(&wide(&name))?;
    let cleanup_event = event(Some(&wide(&format!("{name}-cleanup"))))?;
    let records = mapping.records();
    let mut evidence = Evidence {
        case,
        file: OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{case:?}.jsonl")))?,
    };
    evidence.emit("CaseLaunch", json!({"case_id":case as u32,"frame_hex":"7b2270726f6265223a3135357d0a",
        "expected_bytes":FRAME.len(),"shared_bytes":size_of::<Records>(),"query_cap":QUERY_CAP,
        "setup_ms":SETUP_MS,"observation_ms":OBSERVATION_MS,"candidate_action_ms":ACTION_MS,
        "whole_case_ms":CASE_MS,"cleanup_ms":CLEANUP_MS,"reap_ms":REAP_MS,
        "git_revision":std::env::var("PROBE_GIT_REVISION").ok(),
        "toolchain":std::env::var("RUSTUP_TOOLCHAIN").ok(),"os":std::env::var("PROBE_WINDOWS_BUILD").ok()}))?;
    let log = File::create(directory.join(format!("{case:?}-child.log")))?;
    let mut process = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "windows_transport_probe",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("DOCSBASE_155_CHILD", format!("{case:?}"))
        .env("DOCSBASE_155_MAPPING", &name)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let process_handle = process.as_raw_handle().cast();
    evidence.emit("ChildSpawned", json!({"child_pid":process.id()}))?;
    let mut seen = [false; SLOT_COUNT];
    let mut early_exit = false;
    loop {
        evidence.pump(records, &mut seen, None)?;
        if records.get(Event::CandidateReady).is_some() {
            break;
        }
        let (result, error) = wait(process_handle, 10);
        if result == WAIT_OBJECT_0 {
            early_exit = true;
            break;
        }
        if result != WAIT_TIMEOUT {
            evidence.emit(
                "SupervisorWaitFailure",
                json!({"wait":result,"error":error}),
            )?;
            break;
        }
        if Instant::now() >= case_deadline {
            break;
        }
    }
    evidence.pump(records, &mut seen, None)?;
    let frozen_snapshot = Snapshot::capture(records, early_exit);
    let candidate_good = validate_candidate(&frozen_snapshot, case);
    let candidate_reason = if candidate_good {
        "RequiredCandidateRecords"
    } else if frozen_snapshot.child_exited_before_candidate {
        "ChildExitedBeforeCandidate"
    } else if frozen_snapshot.get(Event::CandidateReady).is_none() {
        "MissingCandidateOrWatchdog"
    } else {
        "CandidateFailed"
    };
    // Sole immutable candidate boundary. The owned snapshot is the only input
    // to validity, reason and payload, and later records cannot change it. This
    // disk/stdout record is flushed before cleanup flag/event, drain, close/kill.
    evidence.emit(
        "CandidateFrozen",
        json!({"valid":candidate_good,
        "reason":candidate_reason, "snapshot":frozen_snapshot.to_json()}),
    )?;
    evidence.emit("CleanupBegin", json!({}))?;
    records.cleanup.store(1, SeqCst);
    signal(raw(&cleanup_event))?;
    let cleanup_deadline = (Instant::now() + Duration::from_millis(CLEANUP_MS)).min(suite_deadline);
    let mut signaled = false;
    while Instant::now() < cleanup_deadline {
        evidence.pump(records, &mut seen, Some(&frozen_snapshot))?;
        let (result, error) = wait(process_handle, 10);
        if result == WAIT_OBJECT_0 {
            signaled = true;
            break;
        }
        if result != WAIT_TIMEOUT {
            evidence.emit("CleanupWaitFailure", json!({"wait":result,"error":error}))?;
            break;
        }
    }
    let mut forced = false;
    if !signaled {
        forced = true;
        evidence.emit(
            "TerminateRequested",
            json!({"child_pid":process.id(),"forced_failure":true}),
        )?;
        // SAFETY: owned Child process handle, still live and never reused; only
        // this supervisor's child is targeted, once. This is asynchronous and
        // pending I/O may prevent exit. No finite kernel cleanup is inferred.
        let (ok, error) = status(unsafe { TerminateProcess(process_handle, 155) });
        evidence.emit("TerminateReturned", json!({"bool":ok,"error":error}))?;
        let reap_deadline = (Instant::now() + Duration::from_millis(REAP_MS)).min(suite_deadline);
        while Instant::now() < reap_deadline {
            evidence.pump(records, &mut seen, Some(&frozen_snapshot))?;
            let (result, error) = wait(process_handle, 10);
            if result == WAIT_OBJECT_0 {
                signaled = true;
                break;
            }
            if result != WAIT_TIMEOUT {
                evidence.emit("ReapWaitFailure", json!({"wait":result,"error":error}))?;
                break;
            }
        }
    }
    evidence.pump(records, &mut seen, Some(&frozen_snapshot))?;
    if !signaled {
        evidence.emit(
            "ChildNotReaped",
            json!({"failure":true,"snapshot":Snapshot::capture(records, early_exit).to_json(),
            "disposition":"StopSuiteAndDisposeHostedRunner"}),
        )?;
        return Err(io::Error::other("ChildNotReaped; no more cases permitted"));
    }
    let exit = process
        .try_wait()?
        .ok_or_else(|| io::Error::other("signaled child lacks exit status"))?;
    let pass = candidate_good
        && !forced
        && exit.success()
        && records.get(Event::CleanupComplete).is_some()
        && records
            .get(Event::WorkerSignaled)
            .is_some_and(|w| w[0] == u64::from(WAIT_OBJECT_0))
        && records
            .get(Event::ClientSignaled)
            .is_some_and(|c| c[0] == u64::from(WAIT_OBJECT_0))
        && records.duplicate_publication.load(SeqCst) == 0;
    evidence.emit(
        "CaseVerdict",
        json!({"pass":pass,"child_signaled":true,"exit_code":exit.code(),
        "forced":forced,"candidate_unchanged":candidate_good,"snapshot":Snapshot::capture(records, early_exit).to_json()}),
    )?;
    mapping.unmap()?;
    Ok(pass)
}

#[test]
fn windows_transport_probe() {
    if let Ok(value) = std::env::var("DOCSBASE_155_CHILD") {
        let result = Case::parse(&value).and_then(|case| {
            let mapping = std::env::var("DOCSBASE_155_MAPPING").map_err(io::Error::other)?;
            child(case, &mapping)
        });
        // Process teardown, including leaked child-owned fixture storage, is only
        // reached after the parent cleanup gate in normal protocol. Native hangs
        // are bounded externally; no claim that ExitProcess itself must finish.
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    assert_eq!(
        std::env::var("DOCSBASE_155_DISPOSABLE_HOST").as_deref(),
        Ok("github-hosted-windows"),
        "run only after review on the disposable Windows workflow host"
    );
    let directory = PathBuf::from(
        std::env::var_os("DOCSBASE_155_EVIDENCE").expect("evidence directory required"),
    );
    std::fs::create_dir_all(&directory).expect("create evidence directory");
    let suite_deadline = Instant::now() + Duration::from_millis(SUITE_MS);
    for case in Case::ALL {
        assert!(
            Instant::now() < suite_deadline,
            "suite deadline exhausted; stop without another child"
        );
        let pass = run_case(case, &directory, suite_deadline)
            .expect("probe orchestration failure; stop suite");
        assert!(
            pass,
            "{case:?} failed/inconclusive; stop without rerun or another child"
        );
    }
}
