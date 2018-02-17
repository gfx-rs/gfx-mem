use std::any::Any;
use std::fmt::Debug;
use std::ops::Range;

use gfx_hal::{Backend, MemoryTypeId};
use gfx_hal::memory::Requirements;

use {MemoryAllocator, MemoryError, MemorySubAllocator};
use arena::{ArenaAllocator, ArenaBlock};
use block::{Block, RawBlock};
use chunked::{ChunkedAllocator, ChunkedBlock};
use root::RootAllocator;

/// Controls what sub allocator is used for an allocation by `CombinedAllocator`
#[derive(Clone, Copy, Debug)]
pub enum Type {
    /// For short-lived objects, such as staging buffers.
    ShortLived,

    /// General purpose.
    General,
}

/// Combines `ArenaAllocator` and `ChunkedAllocator`, and allows the user to control which type of
/// allocation to use.
///
/// Use `RootAllocator` as the super allocator, which will handle the actual memory allocations
/// from `Device`.
///
/// ### Type parameters:
///
/// - `B`: hal `Backend`
#[derive(Debug)]
pub struct CombinedAllocator<B>
where
    B: Backend,
{
    root: RootAllocator<B>,
    arenas: ArenaAllocator<RawBlock<B::Memory>>,
    chunks: ChunkedAllocator<RawBlock<B::Memory>>,
    allocations: usize,
}

impl<B> CombinedAllocator<B>
where
    B: Backend,
{
    /// Create a combined allocator.
    ///
    /// ### Parameters:
    ///
    /// - `memory_type_id`: hal memory type
    /// - `arena_size`: see `ArenaAllocator`
    /// - `blocks_per_chunk`: see `ChunkedAllocator`
    /// - `min_block_size`: see `ChunkedAllocator`
    /// - `max_chunk_size`: see `ChunkedAllocator`
    pub fn new(
        memory_type_id: MemoryTypeId,
        arena_size: u64,
        blocks_per_chunk: usize,
        min_block_size: u64,
        max_chunk_size: u64,
    ) -> Self {
        CombinedAllocator {
            root: RootAllocator::new(memory_type_id),
            arenas: ArenaAllocator::new(arena_size, memory_type_id),
            chunks: ChunkedAllocator::new(
                blocks_per_chunk,
                min_block_size,
                max_chunk_size,
                memory_type_id,
            ),
            allocations: 0,
        }
    }

    /// Get memory type id
    pub fn memory_type(&self) -> MemoryTypeId {
        self.root.memory_type()
    }
}

impl<B> MemoryAllocator<B> for CombinedAllocator<B>
where
    B: Backend,
{
    type Request = Type;
    type Block = CombinedBlock<B::Memory>;

    fn alloc(
        &mut self,
        device: &B::Device,
        request: Type,
        reqs: Requirements,
    ) -> Result<CombinedBlock<B::Memory>, MemoryError> {
        let block = match request {
            Type::ShortLived => self.arenas
                .alloc(&mut self.root, device, (), reqs)
                .map(|ArenaBlock(block, tag)| CombinedBlock(block, CombinedTag::Arena(tag))),
            Type::General => {
                if reqs.size > self.chunks.max_chunk_size() {
                    self.root
                        .alloc(device, (), reqs)
                        .map(|block| CombinedBlock(block, CombinedTag::Root))
                } else {
                    self.chunks.alloc(&mut self.root, device, (), reqs).map(
                        |ChunkedBlock(block, tag)| CombinedBlock(block, CombinedTag::Chunked(tag)),
                    )
                }
            }
        }?;
        self.allocations += 1;
        Ok(block)
    }

    fn free(&mut self, device: &B::Device, block: CombinedBlock<B::Memory>) {
        match block.1 {
            CombinedTag::Arena(tag) => {
                self.arenas
                    .free(&mut self.root, device, ArenaBlock(block.0, tag))
            }
            CombinedTag::Chunked(tag) => {
                self.chunks
                    .free(&mut self.root, device, ChunkedBlock(block.0, tag))
            }
            CombinedTag::Root => self.root.free(device, block.0),
        }
        self.allocations -= 1;
    }

    fn is_used(&self) -> bool {
        if self.allocations == 0 {
            debug_assert!(!self.arenas.is_used() && !self.chunks.is_used());
            true
        } else {
            false
        }
    }

    fn dispose(mut self, device: &B::Device) -> Result<(), Self> {
        if self.is_used() {
            return Err(self);
        }
        self.arenas.dispose(&mut self.root, device).unwrap();
        self.chunks.dispose(&mut self.root, device).unwrap();
        self.root.dispose(device).unwrap();
        Ok(())
    }
}

/// `Block` type returned by `CombinedAllocator`.
#[derive(Debug)]
pub struct CombinedBlock<M>(pub(crate) RawBlock<M>, pub(crate) CombinedTag);

#[derive(Debug)]
pub(crate) enum CombinedTag {
    Arena(u64),
    Chunked(usize),
    Root,
}

impl<M> Block for CombinedBlock<M>
where
    M: Debug + Any,
{
    type Memory = M;

    #[inline(always)]
    fn memory(&self) -> &M {
        self.0.memory()
    }

    #[inline(always)]
    fn range(&self) -> Range<u64> {
        self.0.range()
    }
}

#[test]
#[allow(dead_code)]
fn test_send_sync() {
    fn foo<T: Send + Sync>() {}
    fn bar<B: Backend>() {
        foo::<CombinedAllocator<B>>()
    }
}
