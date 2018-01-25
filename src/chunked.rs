use std::collections::VecDeque;

use gfx_hal::{Backend, MemoryTypeId};
use gfx_hal::memory::Requirements;

use {shift_for_alignment, Block, MemoryError, MemoryAllocator, MemorySubAllocator};

#[derive(Debug)]
struct ChunkedNode<B: Backend, A: MemoryAllocator<B>> {
    id: MemoryTypeId,
    chunks_per_block: usize,
    chunk_size: u64,
    free: VecDeque<(usize, u64)>,
    blocks: Vec<(Block<B, A::Tag>, u64)>,
}

impl<B, A> ChunkedNode<B, A>
where
    B: Backend,
    A: MemoryAllocator<B>,
{
    fn new(chunk_size: u64, chunks_per_block: usize, id: MemoryTypeId) -> Self {
        ChunkedNode {
            id,
            chunk_size,
            chunks_per_block,
            free: VecDeque::new(),
            blocks: Vec::new(),
        }
    }

    fn count(&self) -> usize {
        self.blocks.len() * self.chunks_per_block
    }

    fn grow(&mut self, owner: &mut A, device: &B::Device, info: A::Info) -> Result<(), MemoryError> {
        let reqs = Requirements {
            type_mask: 1 << self.id.0,
            size: self.chunk_size * self.chunks_per_block as u64,
            alignment: self.chunk_size,
        };
        let block = owner.alloc(device, info, reqs)?;
        let offset = shift_for_alignment(reqs.alignment, block.range().start);

        assert!(self.chunks_per_block as u64 <= (block.size() - offset) / self.chunk_size);

        for i in 0..self.chunks_per_block as u64 {
            self.free.push_back((self.blocks.len(), i));
        }
        self.blocks.push((block, offset));

        Ok(())
    }

    fn alloc_no_grow(&mut self) -> Option<Block<B, usize>> {
        self.free.pop_front().map(|(block_index, chunk_index)| {
            let offset = self.blocks[block_index].1 + chunk_index * self.chunk_size;
            let block = Block::new(
                self.blocks[block_index].0.memory(),
                offset..self.chunk_size + offset,
            );
            block.set_tag(block_index)
        })
    }
}

impl<B, A> MemorySubAllocator<B> for ChunkedNode<B, A>
where
    B: Backend,
    A: MemoryAllocator<B>,
{
    type Owner = A;
    type Info = A::Info;
    type Tag = usize;

    fn alloc(
        &mut self,
        owner: &mut A,
        device: &B::Device,
        info: A::Info,
        reqs: Requirements,
    ) -> Result<Block<B, usize>, MemoryError> {
        if (1 << self.id.0) & reqs.type_mask == 0 {
            return Err(MemoryError::NoCompatibleMemoryType);
        }
        assert!(self.chunk_size >= reqs.size);
        assert!(self.chunk_size >= reqs.alignment);
        if let Some(block) = self.alloc_no_grow() {
            Ok(block)
        } else {
            self.grow(owner, device, info)?;
            Ok(self.alloc_no_grow().unwrap())
        }
    }

    fn free(&mut self, _owner: &mut A, _device: &B::Device, block: Block<B, usize>) {
        assert_eq!(block.range().start % self.chunk_size, 0);
        assert_eq!(block.size(), self.chunk_size);
        let offset = block.range().start;
        let block_index = unsafe { block.dispose() };
        let offset = offset - self.blocks[block_index].1;
        let chunk_index = offset / self.chunk_size;
        self.free.push_front((block_index, chunk_index));
    }

    fn is_unused(&self) -> bool {
        self.count() == self.free.len()
    }

    fn dispose(mut self, owner: &mut A, device: &B::Device) -> Result<(), Self> {
        if self.is_unused() {
            for (block, _) in self.blocks.drain(..) {
                owner.free(device, block);
            }
            Ok(())
        } else {
            Err(self)
        }
    }
}


/// Allocator that rounds up requested size to the closes power of 2
/// and returns a block from the list of equal sized chunks.
#[derive(Debug)]
pub struct ChunkedAllocator<B: Backend, A: MemoryAllocator<B>> {
    id: MemoryTypeId,
    chunks_per_block: usize,
    min_chunk_size: u64,
    max_chunk_size: u64,
    nodes: Vec<ChunkedNode<B, A>>,
}

impl<B, A> ChunkedAllocator<B, A>
where
    B: Backend,
    A: MemoryAllocator<B>,
{
    /// Create new chunk-list allocator.
    /// 
    /// # Panics
    ///
    /// Panics if `chunk_size` or `min_chunk_size` are not power of 2.
    ///
    pub fn new(chunks_per_block: usize, min_chunk_size: u64, max_chunk_size: u64, id: MemoryTypeId) -> Self {
        ChunkedAllocator {
            id,
            chunks_per_block,
            min_chunk_size,
            max_chunk_size,
            nodes: Vec::new(),
        }
    }

    /// Get memory type of the allocator
    pub fn memory_type(&self) -> MemoryTypeId {
        self.id
    }

    /// Get chunks per block count
    pub fn chunks_per_block(&self) -> usize {
        self.chunks_per_block
    }

    /// Get minimum chunk size.
    pub fn min_chunk_size(&self) -> u64 {
        self.min_chunk_size
    }

    /// Get maximum chunk size.
    pub fn max_chunk_size(&self) -> u64 {
        self.max_chunk_size
    }

    fn pick_node(&self, size: u64) -> u8 {
        debug_assert!(size <= self.max_chunk_size);
        let bits = ::std::mem::size_of::<usize>() * 8;
        assert!(size != 0);
        (bits - ((size - 1) / self.min_chunk_size).leading_zeros() as usize) as u8
    }

    fn grow(&mut self, size: u8) {
        let Self {
            min_chunk_size,
            max_chunk_size,
            chunks_per_block,
            id,
            ..
        } = *self;

        let chunk_size = |index: u8| min_chunk_size * (1u64 << (index as u8));
        assert!(chunk_size(size) <= max_chunk_size);
        let len = self.nodes.len() as u8;
        self.nodes.extend(
            (len .. size + 1).map(|index| ChunkedNode::new(chunk_size(index), chunks_per_block, id))
        );
    }
}

impl<B, A> MemorySubAllocator<B> for ChunkedAllocator<B, A>
where
    B: Backend,
    A: MemoryAllocator<B>,
{
    type Owner = A;
    type Info = A::Info;
    type Tag = usize;

    fn alloc(
        &mut self,
        owner: &mut A,
        device: &B::Device,
        info: A::Info,
        reqs: Requirements,
    ) -> Result<Block<B, usize>, MemoryError> {
        if reqs.size > self.max_chunk_size {
            return Err(MemoryError::OutOfMemory);
        }
        let index = self.pick_node(reqs.size);
        self.grow(index + 1);
        self.nodes[index as usize].alloc(owner, device, info, reqs)
    }

    fn free(&mut self, owner: &mut A, device: &B::Device, block: Block<B, usize>) {
        let index = self.pick_node(block.size());
        self.nodes[index as usize].free(owner, device, block);
    }

    fn is_unused(&self) -> bool {
        self.nodes.iter().all(ChunkedNode::is_unused)
    }

    fn dispose(mut self, owner: &mut A, device: &B::Device) -> Result<(), Self> {
        if self.is_unused() {
            for node in self.nodes.drain(..) {
                node.dispose(owner, device).unwrap();
            }
            Ok(())
        } else {
            Err(self)
        }
    }
}
