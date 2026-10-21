//! The crash record a native fault leaves behind. A driver fault can leave the
//! heap or the loader locked, so the exception filter only formats fixed-size
//! text on the stack and writes it to a file handle opened at startup; the
//! reporter process or the next launch turns it into the log line, the upload
//! event and the message.

/// `EMCRASH1 <code> <address> <thread> <window shown>\n`, in fixed-width hex.
pub(crate) const RECORD_LEN: usize = 46;
#[cfg(any(windows, test))]
const PREFIX: &[u8; 9] = b"EMCRASH1 ";
/// The reporter appends this once it has saved the report.
pub(crate) const ACK: &[u8] = b"ACK\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub code: u32,
    pub address: u64,
    pub thread: u32,
    pub window_shown: bool,
}

/// Format without allocating; safe to call from the exception filter.
#[cfg(any(windows, test))]
pub(crate) fn format(record: Record, out: &mut [u8; RECORD_LEN]) {
    fn hex(value: u64, digits: usize, out: &mut [u8]) {
        for (i, byte) in out[..digits].iter_mut().enumerate() {
            let nibble = (value >> ((digits - 1 - i) * 4)) & 0xF;
            *byte = b"0123456789ABCDEF"[nibble as usize];
        }
    }
    out[..9].copy_from_slice(PREFIX);
    hex(u64::from(record.code), 8, &mut out[9..17]);
    out[17] = b' ';
    hex(record.address, 16, &mut out[18..34]);
    out[34] = b' ';
    hex(u64::from(record.thread), 8, &mut out[35..43]);
    out[43] = b' ';
    out[44] = if record.window_shown { b'1' } else { b'0' };
    out[45] = b'\n';
}

/// The record at the start of a crash file, and whether it was acknowledged.
pub(crate) fn parse(bytes: &[u8]) -> Option<(Record, bool)> {
    let text = std::str::from_utf8(bytes.get(..RECORD_LEN)?).ok()?;
    let mut fields = text.strip_prefix("EMCRASH1 ")?.trim_end().split(' ');
    let code = u32::from_str_radix(fields.next()?, 16).ok()?;
    let address = u64::from_str_radix(fields.next()?, 16).ok()?;
    let thread = u32::from_str_radix(fields.next()?, 16).ok()?;
    let window_shown = match fields.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let record = Record {
        code,
        address,
        thread,
        window_shown,
    };
    Some((record, bytes[RECORD_LEN..].starts_with(ACK)))
}

#[cfg(windows)]
pub(crate) mod filter {
    //! The unhandled-exception filter. Everything here runs on a thread that
    //! has just faulted: atomics, a stack buffer and direct kernel calls only.

    use super::{format, Record, RECORD_LEN};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    static FILE: AtomicIsize = AtomicIsize::new(0);
    static ENTERED: AtomicBool = AtomicBool::new(false);
    /// How long the faulting process stays alive for the reporter to read
    /// its module list.
    const ACK_WAIT_MS: u32 = 5_000;

    #[link(name = "kernel32")]
    extern "system" {
        fn WriteFile(
            file: isize,
            data: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
        fn FlushFileBuffers(file: isize) -> i32;
        fn GetFileSizeEx(file: isize, size: *mut i64) -> i32;
        fn GetCurrentThreadId() -> u32;
        fn Sleep(milliseconds: u32);
    }

    /// Take ownership of the crash file and install the filter.
    pub(crate) fn install(file: std::fs::File) {
        use std::os::windows::io::IntoRawHandle;
        FILE.store(file.into_raw_handle() as isize, Ordering::SeqCst);
        unsafe {
            windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                on_exception,
            ));
        }
    }

    unsafe extern "system" fn on_exception(
        info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        // EXCEPTION_CONTINUE_SEARCH: Windows then ends the process and records it.
        const CONTINUE_SEARCH: i32 = 0;
        let file = FILE.load(Ordering::SeqCst);
        if file == 0 || ENTERED.swap(true, Ordering::SeqCst) {
            return CONTINUE_SEARCH;
        }
        let Some(exception) = info.as_ref().and_then(|info| info.ExceptionRecord.as_ref()) else {
            return CONTINUE_SEARCH;
        };
        let mut buffer = [0u8; RECORD_LEN];
        format(
            Record {
                code: exception.ExceptionCode.0 as u32,
                address: exception.ExceptionAddress as u64,
                thread: GetCurrentThreadId(),
                window_shown: super::super::window_shown(),
            },
            &mut buffer,
        );
        let mut written = 0u32;
        WriteFile(
            file,
            buffer.as_ptr(),
            RECORD_LEN as u32,
            &mut written,
            core::ptr::null_mut(),
        );
        FlushFileBuffers(file);
        let mut waited = 0;
        while waited < ACK_WAIT_MS {
            let mut size = 0i64;
            if GetFileSizeEx(file, &mut size) != 0 && size > RECORD_LEN as i64 {
                break;
            }
            Sleep(50);
            waited += 50;
        }
        CONTINUE_SEARCH
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_at_a_fixed_width() {
        let record = Record {
            code: 0xC000_0005,
            address: 0x0000_7FF8_1234_5678,
            thread: 0x1A2B,
            window_shown: false,
        };
        let mut buffer = [0u8; RECORD_LEN];
        format(record, &mut buffer);
        assert_eq!(&buffer, b"EMCRASH1 C0000005 00007FF812345678 00001A2B 0\n");
        assert_eq!(parse(&buffer), Some((record, false)));
        let mut acknowledged = buffer.to_vec();
        acknowledged.extend_from_slice(ACK);
        assert_eq!(parse(&acknowledged), Some((record, true)));
    }

    #[test]
    fn empty_partial_and_foreign_files_are_not_records() {
        let mut buffer = [0u8; RECORD_LEN];
        format(
            Record {
                code: 1,
                address: 2,
                thread: 3,
                window_shown: true,
            },
            &mut buffer,
        );
        assert_eq!(parse(b""), None);
        assert_eq!(parse(&buffer[..RECORD_LEN - 1]), None);
        assert_eq!(parse(&[b'x'; RECORD_LEN]), None);
        assert!(parse(&buffer).unwrap().0.window_shown);
    }
}
