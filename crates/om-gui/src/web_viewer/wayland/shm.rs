//! `wl_shm` for the page host's buffers, read through each pool's file
//! (`pread`) rather than mapped: mapping memory another process can shrink
//! or rewrite needs `unsafe` code, and a short read is an error, not a
//! fault. Only the formats GTK draws in are offered.

use std::fs::File;
use std::os::unix::fs::FileExt as _;
use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_server::protocol::wl_buffer::{self, WlBuffer};
use smithay::reexports::wayland_server::protocol::wl_shm::{self, Format, WlShm};
use smithay::reexports::wayland_server::protocol::wl_shm_pool::{self, WlShmPool};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

/// The largest buffer side and size taken (a page as large as a 16k
/// display at twice its scale is not a web viewer).
const MAX_SIDE: i32 = 16_384;
const MAX_BYTES: i64 = 512 * 1024 * 1024;

/// A client's pool: its file, and the size it says the file has.
pub(crate) struct Pool {
    file: File,
    size: Mutex<i32>,
}

/// A buffer in a pool.
pub(crate) struct Buffer {
    pool: Arc<Pool>,
    offset: i32,
    pub width: i32,
    pub height: i32,
    stride: i32,
    format: Format,
}

impl Buffer {
    /// The buffer's pixels as premultiplied RGBA rows (what GTK writes in
    /// ARGB8888 is premultiplied already; XRGB8888 is opaque); `None` when
    /// the pool's file does not hold them.
    pub fn rgba(&self) -> Option<Vec<u8>> {
        let (width, height, stride) = (
            usize::try_from(self.width).ok()?,
            usize::try_from(self.height).ok()?,
            usize::try_from(self.stride).ok()?,
        );
        let row = width * 4;
        let mut bytes = vec![0; stride.checked_mul(height - 1)?.checked_add(row)?];
        self.pool
            .file
            .read_exact_at(&mut bytes, u64::try_from(self.offset).ok()?)
            .ok()?;
        let opaque = self.format == Format::Xrgb8888;
        let mut rgba = Vec::with_capacity(row * height);
        for y in 0..height {
            for px in bytes[y * stride..y * stride + row].chunks_exact(4) {
                // Little-endian ARGB8888: B, G, R, A.
                rgba.extend_from_slice(&[px[2], px[1], px[0], if opaque { 255 } else { px[3] }]);
            }
        }
        Some(rgba)
    }
}

/// The `wl_shm` global's state.
pub(crate) struct Shm;

impl<D> GlobalDispatch<WlShm, (), D> for Shm
where
    D: GlobalDispatch<WlShm, ()> + Dispatch<WlShm, ()> + Dispatch<WlShmPool, Arc<Pool>> + 'static,
{
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WlShm>,
        _: &(),
        init: &mut DataInit<'_, D>,
    ) {
        let shm = init.init(resource, ());
        shm.format(Format::Argb8888);
        shm.format(Format::Xrgb8888);
    }
}

impl<D> Dispatch<WlShm, (), D> for Shm
where
    D: Dispatch<WlShm, ()> + Dispatch<WlShmPool, Arc<Pool>> + 'static,
{
    fn request(
        _: &mut D,
        _: &Client,
        shm: &WlShm,
        request: wl_shm::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, D>,
    ) {
        if let wl_shm::Request::CreatePool { id, fd, size } = request {
            if size <= 0 {
                shm.post_error(wl_shm::Error::InvalidStride, "invalid pool size");
                return;
            }
            init.init(
                id,
                Arc::new(Pool {
                    file: File::from(fd),
                    size: Mutex::new(size),
                }),
            );
        }
    }
}

impl<D> Dispatch<WlShmPool, Arc<Pool>, D> for Shm
where
    D: Dispatch<WlShmPool, Arc<Pool>> + Dispatch<WlBuffer, Buffer> + 'static,
{
    fn request(
        _: &mut D,
        _: &Client,
        resource: &WlShmPool,
        request: wl_shm_pool::Request,
        pool: &Arc<Pool>,
        _: &DisplayHandle,
        init: &mut DataInit<'_, D>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                let format = match format {
                    WEnum::Value(f @ (Format::Argb8888 | Format::Xrgb8888)) => f,
                    _ => {
                        resource.post_error(wl_shm::Error::InvalidFormat, "unsupported format");
                        return;
                    }
                };
                let size = i64::from(*pool.size.lock().unwrap());
                let end = i64::from(offset) + i64::from(stride) * i64::from(height);
                let valid = offset >= 0
                    && (1..=MAX_SIDE).contains(&width)
                    && (1..=MAX_SIDE).contains(&height)
                    && i64::from(stride) >= i64::from(width) * 4
                    && end <= size
                    && end <= MAX_BYTES;
                if !valid {
                    resource.post_error(wl_shm::Error::InvalidStride, "buffer outside its pool");
                    return;
                }
                init.init(
                    id,
                    Buffer {
                        pool: Arc::clone(pool),
                        offset,
                        width,
                        height,
                        stride,
                        format,
                    },
                );
            }
            wl_shm_pool::Request::Resize { size } => {
                let mut current = pool.size.lock().unwrap();
                if size < *current {
                    resource.post_error(wl_shm::Error::InvalidStride, "a pool cannot shrink");
                } else {
                    *current = size;
                }
            }
            _ => {}
        }
    }
}

impl<D> Dispatch<WlBuffer, Buffer, D> for Shm
where
    D: Dispatch<WlBuffer, Buffer> + 'static,
{
    fn request(
        _: &mut D,
        _: &Client,
        _: &WlBuffer,
        _: wl_buffer::Request,
        _: &Buffer,
        _: &DisplayHandle,
        _: &mut DataInit<'_, D>,
    ) {
    }
}
