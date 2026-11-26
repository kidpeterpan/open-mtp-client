//! A sparse file holding only the pieces of a video the phone gave: the file is
//! as long as the original, the pieces sit at their original offsets, and the
//! rest is a hole that costs no disk. A decoder that reads the index at the end
//! and the first picture at the start finds both where the file says they are.

use std::fs::OpenOptions;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

/// `bytes` as they are at `offset` in the original file.
pub struct Piece<'a> {
    pub offset: u64,
    pub bytes: &'a [u8],
}

/// Creates `path` (never over an existing file, readable by its owner only),
/// `len` bytes long, with each piece written at its offset. A piece that does
/// not fit inside `len` is `InvalidInput`, checked before anything is created.
pub fn write_sparse(path: &Path, len: u64, pieces: &[Piece<'_>]) -> io::Result<()> {
    for piece in pieces {
        let fits = u64::try_from(piece.bytes.len())
            .ok()
            .and_then(|size| piece.offset.checked_add(size))
            .is_some_and(|end| end <= len);
        if !fits {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a piece lies outside the file",
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.set_len(len)?;
    for piece in pieces {
        file.seek(SeekFrom::Start(piece.offset))?;
        file.write_all(piece.bytes)?;
    }
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{ErrorKind, Read, Seek, SeekFrom};

    fn read_at(path: &std::path::Path, offset: u64, len: usize) -> Vec<u8> {
        let mut file = fs::File::open(path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut bytes = vec![0; len];
        file.read_exact(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn pieces_land_at_their_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");
        let pieces = [
            Piece {
                offset: 0,
                bytes: b"head",
            },
            Piece {
                offset: 96,
                bytes: b"tail",
            },
        ];

        write_sparse(&path, 100, &pieces).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().len(), 100);
        assert_eq!(read_at(&path, 0, 4), b"head");
        assert_eq!(read_at(&path, 96, 4), b"tail");
    }

    #[test]
    fn the_middle_stays_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");

        write_sparse(
            &path,
            100,
            &[
                Piece {
                    offset: 0,
                    bytes: b"ab",
                },
                Piece {
                    offset: 98,
                    bytes: b"yz",
                },
            ],
        )
        .unwrap();

        assert!(read_at(&path, 2, 96).iter().all(|&byte| byte == 0));
    }

    #[test]
    fn a_piece_past_the_end_is_invalid_input_and_makes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");

        let err = write_sparse(
            &path,
            10,
            &[Piece {
                offset: 8,
                bytes: b"abc",
            }],
        )
        .unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            !path.exists(),
            "nothing is created for a plan that cannot be written"
        );
    }

    #[test]
    fn a_piece_whose_end_overflows_u64_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");

        let err = write_sparse(
            &path,
            u64::MAX,
            &[Piece {
                offset: u64::MAX - 1,
                bytes: b"abc",
            }],
        )
        .unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn an_existing_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");
        fs::write(&path, b"mine").unwrap();

        let err = write_sparse(&path, 10, &[]).unwrap_err();

        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"mine");
    }

    #[test]
    fn a_zero_length_file_with_no_pieces_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");

        write_sparse(&path, 0, &[]).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_big_file_with_small_pieces_takes_little_disk() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");
        let data = vec![7u8; 64 * 1024];

        write_sparse(
            &path,
            256 << 20,
            &[
                Piece {
                    offset: 0,
                    bytes: &data,
                },
                Piece {
                    offset: (256 << 20) - 4096,
                    bytes: &data[..4096],
                },
            ],
        )
        .unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 256 << 20);
        assert!(
            meta.blocks() * 512 < 4 << 20,
            "{} bytes on disk",
            meta.blocks() * 512
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_the_owner_can_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.mp4");

        write_sparse(&path, 10, &[]).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
