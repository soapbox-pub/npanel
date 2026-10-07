//! Large blobs sent with sendfile(2), recognised by where their bytes live.
//!
//! hyper takes a response body as bytes and has no way to be handed a file.
//! But a blob's body is a slice of its kept memory mapping, and hyper writes a
//! body to a TCP connection without copying it: the head and the body's own
//! bytes go to the connection side by side, in one vectored write. So the
//! connection (`server::TcpIo`) asks here whether a slice it is about to write
//! lies inside the mapping of a file, and if it does, has the kernel send that
//! range of the file instead — straight from the page cache to the socket.
//!
//! The bytes are the file's either way, so nothing that keeps a slice from
//! being recognised — TLS, a Tor stream, a hyper that copies bodies — changes
//! what is sent, only how fast. `sends_files_over_tcp` in `server.rs` fails if
//! a hyper upgrade ever stops passing bodies through.

use bytes::Bytes;

/// Size from which a mapping is registered, and so sent with sendfile(2).
///
/// Measured over loopback with four cores saturated: below this, sendfile and
/// a write from the mapping tie, so registering a file would only hold its
/// descriptor open. From here sendfile pulls ahead, by 8% at 64 KiB and
/// 30–55% from 128 KiB to 16 MiB.
pub const MIN: u64 = 64 * 1024;

/// A blob's mapping as response bytes, registered for sendfile(2) when it is
/// large enough to be worth it. Only Linux's connections send files, so only
/// there is one registered, or its file kept open.
pub fn bytes(map: memmap2::Mmap, file: std::fs::File) -> Bytes {
    #[cfg(target_os = "linux")]
    if map.len() as u64 >= MIN {
        return Bytes::from_owner(linux::Registered::new(map, file));
    }
    drop(file);
    Bytes::from_owner(map)
}

#[cfg(target_os = "linux")]
pub use linux::find;

#[cfg(all(test, target_os = "linux"))]
pub use linux::SENT;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::os::fd::{AsRawFd, RawFd};

    use parking_lot::RwLock;

    /// Every registered mapping, by the address it starts at: its length, and
    /// the descriptor of the file it maps.
    pub(super) static MAPPINGS: RwLock<BTreeMap<usize, (usize, RawFd)>> = RwLock::new(BTreeMap::new());

    /// Bytes sent with sendfile(2), for tests to see that they were.
    #[cfg(test)]
    pub static SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// A mapping that is registered for as long as it exists.
    ///
    /// An address is only ever registered while its mapping is: the entry goes
    /// in after the mapping is made and comes out in `drop`, before the fields
    /// unmap it and close the file. So no other mapping can be at a registered
    /// address, and a registered descriptor is open.
    pub struct Registered {
        map: memmap2::Mmap,
        /// Open for sendfile(2) to read; closed after the entry comes out.
        _file: std::fs::File,
    }

    impl Registered {
        pub fn new(map: memmap2::Mmap, file: std::fs::File) -> Registered {
            MAPPINGS.write().insert(map.as_ptr() as usize, (map.len(), file.as_raw_fd()));
            Registered { map, _file: file }
        }
    }

    impl AsRef<[u8]> for Registered {
        fn as_ref(&self) -> &[u8] {
            &self.map
        }
    }

    impl Drop for Registered {
        fn drop(&mut self) {
            MAPPINGS.write().remove(&(self.map.as_ptr() as usize));
        }
    }

    /// The file `slice` is a range of, and where in the file it starts, if it
    /// lies entirely inside a registered mapping.
    ///
    /// The descriptor is open for as long as `slice` is borrowed: memory inside
    /// a registered mapping belongs to that mapping, and whatever lent the
    /// slice holds it, so it can't be dropped underneath the caller.
    pub fn find(slice: &[u8]) -> Option<(RawFd, u64)> {
        let start = slice.as_ptr() as usize;
        let mappings = MAPPINGS.read();
        let (&base, &(len, fd)) = mappings.range(..=start).next_back()?;
        (start + slice.len() <= base + len).then_some((fd, (start - base) as u64))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::Write;

    use super::*;

    fn mapped(len: usize) -> Bytes {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&vec![7; len]).unwrap();
        // SAFETY: nothing else has the temporary file.
        let map = unsafe { memmap2::Mmap::map(&file).unwrap() };
        bytes(map, file)
    }

    #[test]
    fn finds_a_slice_of_a_large_mapping() {
        let large = mapped(MIN as usize);
        let (fd, offset) = find(&large).unwrap();
        assert_eq!(offset, 0);
        assert_eq!(find(&large[100..200]), Some((fd, 100)));
        assert_eq!(find(&large[large.len() - 1..]), Some((fd, MIN - 1)));

        // Not a copy of it, which is somewhere else.
        let copy = large.to_vec();
        assert_eq!(find(&copy), None);
    }

    #[test]
    fn forgets_a_mapping_once_it_is_dropped() {
        let large = mapped(MIN as usize);
        let address = large.as_ptr() as usize;
        let (fd, _) = find(&large).unwrap();
        drop(large);
        // Another test's mapping may take the address straight away, but not
        // with this length and this descriptor too.
        assert_ne!(linux::MAPPINGS.read().get(&address), Some(&(MIN as usize, fd)));
    }

    #[test]
    fn leaves_a_small_mapping_alone() {
        let small = mapped(MIN as usize - 1);
        assert_eq!(find(&small), None);
    }
}
