//! Memory management for gfx_hal.
//!
//! ### Example
//!
//! ```rust
//! extern crate gfx_hal;
//! extern crate gfx_memory;
//! extern crate failure;
//!
//! use failure::Error;
//!
//! use gfx_hal::{Backend, Device};
//! use gfx_hal::buffer::Usage;
//! use gfx_hal::memory::Properties;
//! use gfx_memory::{MemoryAllocator, SmartAllocator, SmartBlock, Type, Block};
//!
//! fn make_vertex_buffer<B: Backend>(device: &B::Device,
//!                                   allocator: &mut SmartAllocator<B>,
//!                                   size: u64
//! ) -> Result<(SmartBlock<B::Memory>, B::Buffer), Error>
//! {
//!     // Create unbounded buffer object. It has no memory assigned.
//!     let mut buf = unsafe { device.create_buffer(size, Usage::VERTEX)? };
//!     // Ger memory requirements for the buffer.
//!     let reqs = unsafe { device.get_buffer_requirements(&buf) };
//!     // Allocate block of device-local memory that satisfy requirements for buffer.
//!     let block = unsafe { allocator.alloc(device, (Type::General, Properties::DEVICE_LOCAL), reqs)? };
//!     // Bind memory block to the buffer.
//!     unsafe { device.bind_buffer_memory(block.memory(), block.range().start, &mut buf)? };
//!     Ok((block, buf))
//! }
//!
//! # fn main() {}
//! ```

#![deny(dead_code)]
#![deny(missing_docs)]
#![deny(unused_imports)]
#![deny(unused_must_use)]

extern crate gfx_hal;
#[macro_use]
extern crate failure;
extern crate relevant;

pub use arena::{ArenaAllocator, ArenaBlock};
pub use block::{Block, RawBlock};
pub use chunked::{ChunkedAllocator, ChunkedBlock};
pub use combined::{CombinedAllocator, CombinedBlock, Type};
pub use factory::{Factory, FactoryError, Item};
pub use root::RootAllocator;
pub use smart::{SmartAllocator, SmartBlock};

use std::cmp::PartialOrd;
use std::fmt::Debug;
use std::ops::{Add, BitOr, Sub};

use gfx_hal::device::AllocationError;
use gfx_hal::device::OutOfMemory;
use gfx_hal::memory::Requirements;
use gfx_hal::Backend;

mod arena;
mod block;
mod chunked;
mod combined;
mod factory;
mod root;
mod smart;

/// Possible errors that may be returned from allocators.
#[derive(Clone, Debug, Fail)]
pub enum MemoryError {
    /// Allocator doesn't have compatible memory types.
    #[fail(display = "No compatible memory found")]
    NoCompatibleMemoryType,

    /// All compatible memory is exhausted.
    #[fail(display = "Out of memory")]
    OutOfMemory,

    /// Implementations might have a limit on number of allocations
    #[fail(display = "Can't allocate more objects")]
    TooManyObjects,
}

impl From<OutOfMemory> for MemoryError {
    fn from(_: OutOfMemory) -> Self {
        MemoryError::OutOfMemory
    }
}

impl From<AllocationError> for MemoryError {
    fn from(err: AllocationError) -> Self {
        match err {
            AllocationError::OutOfMemory(_) => MemoryError::OutOfMemory,
            AllocationError::TooManyObjects => MemoryError::TooManyObjects,
        }
    }
}

/// Trait for managing memory allocations from a `Device`.
///
/// ### Type parameters:
///
/// - `B`: hal `Backend`
pub trait MemoryAllocator<B: Backend>: Debug {
    /// Information required to allocate block.
    type Request;

    /// Allocator will allocate blocks of this type.
    type Block: Block<Memory = B::Memory> + Debug + Send + Sync;

    /// Allocate a block of memory.
    ///
    /// ### Parameters:
    ///
    /// - `device`: device to allocate the memory from, must always be the same for an instance
    ///             of the allocator
    /// - `info`: information required to allocate a block of memory
    /// - `req`: the requirements the memory block must meet
    ///
    /// ### Returns
    ///
    /// Returns a memory block compatible with the given requirements. If no such block could be
    /// allocated, a `MemoryError` is returned.
    unsafe fn alloc(
        &mut self,
        device: &B::Device,
        request: Self::Request,
        reqs: Requirements,
    ) -> Result<Self::Block, MemoryError>;

    /// Free a block of memory.
    ///
    /// The block must be allocated from this allocator.
    ///
    /// ### Parameters:
    ///
    /// - `device`: same device that was used to allocate the block of memory
    /// - `block`: block of memory to free
    unsafe fn free(&mut self, device: &B::Device, block: Self::Block);

    /// Check if any of the blocks allocated by this allocator are still in use.
    /// If this function returns `false`, the allocator can be `dispose`d.
    fn is_used(&self) -> bool;

    /// Attempt to dispose of this allocator.
    ///
    /// Allocators must be disposed using this function, dropping them before this might result in
    /// a panic.
    ///
    /// ### Parameters:
    ///
    /// - `device`: must be the same device all allocations have been made against
    ///
    /// ### Returns
    ///
    /// If the allocator contains memory blocks that are still in use, this will return `Err(self)`.
    unsafe fn dispose(self, device: &B::Device) -> Result<(), Self>
    where
        Self: Sized;
}

/// Trait for allocators that sub-allocate memory blocks from another allocator it doesn't own.
pub trait MemorySubAllocator<B: Backend, O> {
    /// Information required to allocate block.
    type Request;

    /// Allocator will allocate blocks of this type.
    type Block: Block<Memory = B::Memory> + Debug + Send + Sync;

    /// Allocate a block of memory from this allocator.
    /// This allocator will use `owner` to allocate memory in bigger chunks.
    ///
    /// ### Parameters:
    ///
    /// - `owner`: allocator used to allocate memory in bigger chunks, must always be the same
    ///            for an instance of this sub allocator
    /// - `device`: device to allocate the memory from, must always be the same for an instance
    ///             of the allocator
    /// - `info`: information required to allocate a block of memory, may contain additional
    ///           requirements and/or hints for allocation.
    /// - `reqs`: the requirements the memory block must meet
    ///
    /// ### Returns
    ///
    /// Returns a memory block compatible with the given requirements. If no such block could be
    /// allocated, a `MemoryError` is returned.
    unsafe fn alloc(
        &mut self,
        owner: &mut O,
        device: &B::Device,
        request: Self::Request,
        reqs: Requirements,
    ) -> Result<Self::Block, MemoryError>;

    /// Free a block of memory.
    ///
    /// The block must be allocated from this allocator. The allocator may choose to free the inner
    /// block of memory allocated from `owner`, if it is no longer in use.
    ///
    /// ### Parameters:
    ///
    /// - `owner`: allocator that was used to allocate the inner memory blocks
    /// - `device`: same device that was used to allocate the block of memory
    /// - `block`: block of memory to free
    unsafe fn free(&mut self, owner: &mut O, device: &B::Device, block: Self::Block);

    /// Attempt to dispose of this allocator.
    ///
    /// Allocators must be disposed using this function, dropping them before this might result in
    /// a panic.
    ///
    /// ### Parameters:
    ///
    /// - `owner`: allocator that was used to allocate the inner memory blocks
    /// - `device`: must be the same device all allocations have been made against
    ///
    /// ### Returns
    ///
    /// If the allocator contains memory blocks that are still in use, this will return `Err(self)`.
    unsafe fn dispose(self, owner: &mut O, device: &B::Device) -> Result<(), Self>
    where
        Self: Sized;
}

/// Calculate shift from specified offset required to satisfy alignment.
pub fn alignment_shift<T>(alignment: T, offset: T) -> T
where
    T: From<u8> + Add<Output = T> + Sub<Output = T> + BitOr<Output = T> + PartialOrd + Copy,
{
    shift_for_alignment(alignment, offset) - offset
}

/// Shift from specified offset to fulfill required alignment
pub fn shift_for_alignment<T>(alignment: T, offset: T) -> T
where
    T: From<u8> + Add<Output = T> + Sub<Output = T> + BitOr<Output = T> + PartialOrd,
{
    if offset > 0.into() && alignment > 0.into() {
        ((offset - 1.into()) | (alignment - 1.into())) + 1.into()
    } else {
        offset
    }
}
