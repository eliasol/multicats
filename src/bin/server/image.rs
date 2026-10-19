use std::{
    fs::File,
    io::{Read, Result, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};

use log::info;
use multicats::{ChunkMetadata, ImageMetadata, sparse};
use twox_hash::XxHash3_64;

const BUFFER_ALIGN: usize = 4096;

pub fn compute_image_metadata(file: impl AsRef<Path>, chunk_size: usize) -> Result<ImageMetadata> {
    let mut file = File::open(file)?;
    let file_size = file.metadata()?.len();

    let data_ranges = sparse::get_data_ranges(&file, file_size)?;
    let total_allocated: u64 = data_ranges.iter().map(|(_, len)| *len).sum();

    info!(
        "Scanning file (size: {} bytes, allocated data: {} bytes across {} extents)...",
        file_size,
        total_allocated,
        data_ranges.len()
    );

    let mut chunk_list: Vec<ChunkMetadata> = Vec::new();

    // Align the read buffer to page boundary.
    // It can speed up read() of 2x.
    // TODO: do not hardcode the page size.
    let mut buf = vec![0u8; chunk_size + BUFFER_ALIGN].into_boxed_slice();
    let ptr = buf.as_ptr() as usize;
    let offset = (BUFFER_ALIGN - (ptr % BUFFER_ALIGN)) % BUFFER_ALIGN;
    let buf = &mut buf[offset..offset + chunk_size];

    let start_time = Instant::now();
    let mut processed_bytes = 0u64;
    let mut last_bytes = 0u64;
    let mut last_report = start_time;

    for &(range_start, range_len) in &data_ranges {
        file.seek(SeekFrom::Start(range_start))?;
        let mut range_pos = 0u64;

        while range_pos < range_len {
            let now = Instant::now();
            if now >= last_report + Duration::from_secs(1) {
                let pct = if total_allocated > 0 {
                    processed_bytes * 100 / total_allocated
                } else {
                    100
                };
                info!(
                    "Computing file metadata... {}% ({:.2} MB/s)",
                    pct,
                    (processed_bytes - last_bytes) as f32
                        / (1024f32 * 1024f32)
                        / (now - last_report).as_secs_f32()
                );
                last_report = now;
                last_bytes = processed_bytes;
            }

            let target_size = (range_len - range_pos).min(chunk_size as u64) as usize;
            let mut count: usize = 0;
            while count < target_size {
                let n = file.read(&mut buf[count..target_size])?;
                if n == 0 {
                    break;
                }
                count += n;
            }

            let hash = XxHash3_64::oneshot(&buf[0..count]);
            chunk_list.push(ChunkMetadata {
                offset: range_start + range_pos,
                size: count,
                hash,
            });

            range_pos += count as u64;
            processed_bytes += count as u64;
        }
    }

    let end_time = Instant::now();

    info!(
        "Image metadata generation completed in {:.2} seconds ({} chunks generated)",
        (end_time - start_time).as_secs_f32(),
        chunk_list.len()
    );

    Ok(ImageMetadata {
        file_size,
        chunks: chunk_list.into_boxed_slice(),
    })
}
