use std::{
    fs::File,
    io::{Read, Result},
    path::Path,
    time::{Duration, Instant},
};

use log::info;
use multicats::{ChunkMetadata, ImageMetadata};
use twox_hash::XxHash3_64;

const BUFFER_ALIGN: usize = 4096;

pub fn compute_image_metadata(file: impl AsRef<Path>, chunk_size: usize) -> Result<ImageMetadata> {
    let mut file = File::open(file)?;
    let file_size = file.metadata()?.len();

    let mut chunk_list: Vec<ChunkMetadata> = Vec::new();

    // Align the read buffer to page boundary.
    // It can speed up read() of 2x.
    // TODO: do not hardcode the page size.
    let mut buf = vec![0u8; chunk_size + BUFFER_ALIGN].into_boxed_slice();
    let ptr = buf.as_ptr() as usize;
    let offset = (BUFFER_ALIGN - (ptr % BUFFER_ALIGN)) % BUFFER_ALIGN;
    let buf = &mut buf[offset..offset + chunk_size];

    let mut pos = 0u64;

    let start_time = Instant::now();
    let mut last_pos = pos;
    let mut last_report = start_time;

    while pos < file_size {
        let now = Instant::now();
        if now >= last_report + Duration::from_secs(1) {
            info!(
                "Computing file metadata... {}% ({} MB/s)",
                pos * 100 / file_size,
                (pos - last_pos) as f32 / (1024f32 * 1024f32) / (now - last_report).as_secs_f32()
            );
            last_report = now;
            last_pos = pos;
        }
        let target_size = (file_size - pos).min(chunk_size as u64) as usize;
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
            offset: pos,
            size: count,
            hash,
        });
        pos += count as u64;
    }

    let end_time = Instant::now();

    info!(
        "Image metadata generation completed in {} seconds",
        (end_time - start_time).as_secs_f32()
    );

    Ok(ImageMetadata {
        chunks: chunk_list.into_boxed_slice(),
    })
}
