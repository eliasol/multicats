use std::collections::BTreeSet;

use anyhow::{Result, bail};

pub struct ChunkAssembler {
    data: Vec<u8>,
    map: BTreeSet<(usize, usize)>,
}

impl ChunkAssembler {
    pub fn new(chunk_size: usize) -> ChunkAssembler {
        ChunkAssembler {
            data: vec![0u8; chunk_size],
            map: BTreeSet::new(),
        }
    }

    pub fn add_fragment(&mut self, offset: usize, data: &[u8]) -> Result<()> {
        let before = self
            .map
            .range(..(offset, 0))
            .next_back()
            .copied()
            .unwrap_or((0, 0));
        let after = self
            .map
            .range((offset, 0)..)
            .next()
            .copied()
            .unwrap_or((self.data.len(), 0));

        let mut add = (offset, data.len());

        if before.0 + before.1 > add.0 || add.0 + add.1 > after.0 {
            bail!("Fragment overlaps with data or is outside of chunk boundaries");
        }

        let _ = &self.data[add.0..add.0 + add.1].copy_from_slice(data);

        if before.0 + before.1 == add.0 {
            self.map.remove(&before);
            add.0 = before.0;
            add.1 += before.1;
        }

        if add.0 + add.1 == after.0 {
            self.map.remove(&after);
            add.1 += after.1;
        }

        self.map.insert(add);

        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.map.len() == 1 && *self.map.first().unwrap() == (0, self.data.len())
    }

    pub fn complete(self) -> Vec<u8> {
        if !self.is_complete() {
            panic!("Tried to complete an incomplete chunk");
        }

        self.data
    }
}
