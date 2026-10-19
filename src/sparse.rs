use std::fs::File;
use std::io::Result;

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

/// Query all allocated data ranges for a file.
/// Returns a vector of `(offset, length)` tuples covering regions with data.
/// Unallocated regions ("holes") are omitted.
///
/// Falls back to returning the full `[(0, file_size)]` range if sparse querying
/// is not supported by the underlying filesystem.
#[cfg(unix)]
pub fn get_data_ranges(file: &File, file_size: u64) -> Result<Vec<(u64, u64)>> {
    use rustix::fs::{seek, SeekFrom};

    if file_size == 0 {
        return Ok(Vec::new());
    }

    let mut ranges = Vec::new();
    let mut offset = 0u64;

    while offset < file_size {
        // SEEK_DATA finds the next data location >= offset
        let data_start = match seek(file, SeekFrom::Data(offset)) {
            Ok(pos) => pos,
            Err(rustix::io::Errno::NXIO) => {
                // No more data in the file; remaining bytes are a hole
                break;
            }
            Err(rustix::io::Errno::INVAL) => {
                // Filesystem does not support SEEK_DATA, fall back to entire file
                return Ok(vec![(0, file_size)]);
            }
            Err(e) => return Err(e.into()),
        };

        if data_start >= file_size {
            break;
        }

        // SEEK_HOLE finds the next hole location >= data_start (or file_size if no further holes)
        let hole_start = match seek(file, SeekFrom::Hole(data_start)) {
            Ok(pos) => pos.min(file_size),
            Err(rustix::io::Errno::INVAL) => {
                return Ok(vec![(0, file_size)]);
            }
            Err(e) => return Err(e.into()),
        };

        if hole_start > data_start {
            ranges.push((data_start, hole_start - data_start));
        }

        offset = hole_start;
    }

    Ok(ranges)
}

#[cfg(windows)]
pub fn get_data_ranges(file: &File, file_size: u64) -> Result<Vec<(u64, u64)>> {
    if file_size == 0 {
        return Ok(Vec::new());
    }

    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, HANDLE};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES,
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut ranges = Vec::new();
    let mut query_offset = 0i64;
    let file_size_i64 = file_size as i64;

    while query_offset < file_size_i64 {
        let mut query = FILE_ALLOCATED_RANGE_BUFFER {
            FileOffset: query_offset,
            Length: file_size_i64 - query_offset,
        };

        let mut out_buffer = vec![FILE_ALLOCATED_RANGE_BUFFER { FileOffset: 0, Length: 0 }; 512];
        let mut bytes_returned = 0u32;

        // SAFETY:
        // - `handle` is derived from an open, valid `std::fs::File` borrow that lives for the duration of this call.
        // - `query` is an aligned stack-allocated `FILE_ALLOCATED_RANGE_BUFFER` containing valid query bounds.
        // - `out_buffer` is a heap vector allocated with initialized structs matching `sizeof(FILE_ALLOCATED_RANGE_BUFFER) * len`.
        // - `bytes_returned` is a valid pointer to a stack u32 that receives the number of bytes written.
        // - `lpOverlapped` is NULL because synchronous file I/O is used.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_ALLOCATED_RANGES,
                &mut query as *mut _ as *const _,
                std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
                out_buffer.as_mut_ptr() as *mut _,
                (out_buffer.len() * std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>()) as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Ok(vec![(0, file_size)]);
            }
        }

        let num_elements =
            (bytes_returned as usize) / std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
        if num_elements == 0 {
            break;
        }

        for i in 0..num_elements {
            let r = &out_buffer[i];
            if r.Length > 0 {
                ranges.push((r.FileOffset as u64, r.Length as u64));
            }
        }

        let last = &out_buffer[num_elements - 1];
        let next_offset = last.FileOffset + last.Length;
        if next_offset <= query_offset {
            break;
        }
        query_offset = next_offset;
    }

    Ok(ranges)
}

/// Fallback for targets other than Unix and Windows.
#[cfg(not(any(unix, windows)))]
pub fn get_data_ranges(_file: &File, file_size: u64) -> Result<Vec<(u64, u64)>> {
    if file_size == 0 {
        Ok(Vec::new())
    } else {
        Ok(vec![(0, file_size)])
    }
}

/// Mark a newly created file as sparse on filesystems that require explicit initialization (Windows).
/// On Linux/Unix, files are inherently sparse when seeking/extending, so this is a no-op.
#[cfg(not(windows))]
pub fn set_sparse_file(_file: &tokio::fs::File) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn set_sparse_file(file: &tokio::fs::File) -> Result<()> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let handle = file.as_raw_handle() as HANDLE;
    let mut bytes_returned = 0u32;

    // SAFETY:
    // - `handle` is obtained from a valid open `tokio::fs::File` with write permissions.
    // - According to Win32 documentation for FSCTL_SET_SPARSE, passing NULL for lpInBuffer
    //   and lpOutBuffer (with lengths 0) defaults to setting the sparse attribute to TRUE on the file.
    // - `bytes_returned` receives output bytes count.
    // - `lpOverlapped` is NULL for synchronous execution.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if ok == 0 {
        let err = std::io::Error::last_os_error();
        log::warn!("Could not mark file as sparse: {}", err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn test_empty_file_ranges() {
        let path = std::env::temp_dir().join("multicats_test_empty.bin");
        let file = File::create(&path).unwrap();
        let ranges = get_data_ranges(&file, 0).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(ranges.is_empty());
    }

    #[test]
    fn test_dense_file_ranges() {
        let path = std::env::temp_dir().join("multicats_test_dense.bin");
        let mut file = File::create(&path).unwrap();
        file.write_all(&[42u8; 1024]).unwrap();
        let ranges = get_data_ranges(&file, 1024).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(ranges, vec![(0, 1024)]);
    }

    #[test]
    #[cfg(unix)]
    fn test_sparse_file_ranges() {
        let path = std::env::temp_dir().join("multicats_test_sparse.bin");
        let mut file = File::create(&path).unwrap();
        file.write_all(&[1u8; 4096]).unwrap();
        file.seek(SeekFrom::Start(1024 * 1024)).unwrap();
        file.write_all(&[2u8; 4096]).unwrap();
        let file_size = file.metadata().unwrap().len();
        let ranges = get_data_ranges(&file, file_size).unwrap();
        let _ = std::fs::remove_file(&path);

        if ranges.len() > 1 {
            assert_eq!(ranges[0].0, 0);
            assert!(ranges.last().unwrap().0 >= 1024 * 1024);
            let total_allocated: u64 = ranges.iter().map(|(_, len)| *len).sum();
            assert!(total_allocated < file_size);
        } else {
            assert_eq!(ranges, vec![(0, file_size)]);
        }
    }
}
