use sha2::Digest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
    Crc32,
}

impl HashAlgo {
    pub fn all() -> &'static [HashAlgo] {
        &[HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Crc32]
    }

    pub fn label(&self) -> &'static str {
        match self {
            HashAlgo::Md5 => "MD5",
            HashAlgo::Sha1 => "SHA-1",
            HashAlgo::Sha256 => "SHA-256",
            HashAlgo::Crc32 => "CRC32",
        }
    }
}

/// Incremental hasher; feed chunks with `update`, then `finish` for lowercase hex.
pub struct StreamHasher {
    state: State,
}

enum State {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Crc32(crc32fast::Hasher),
}

impl StreamHasher {
    pub fn new(algo: HashAlgo) -> StreamHasher {
        let state = match algo {
            HashAlgo::Md5 => State::Md5(md5::Md5::new()),
            HashAlgo::Sha1 => State::Sha1(sha1::Sha1::new()),
            HashAlgo::Sha256 => State::Sha256(sha2::Sha256::new()),
            HashAlgo::Crc32 => State::Crc32(crc32fast::Hasher::new()),
        };
        StreamHasher { state }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match &mut self.state {
            State::Md5(h) => h.update(bytes),
            State::Sha1(h) => h.update(bytes),
            State::Sha256(h) => h.update(bytes),
            State::Crc32(h) => h.update(bytes),
        }
    }

    pub fn finish(self) -> String {
        match self.state {
            State::Md5(h) => to_hex(&h.finalize()),
            State::Sha1(h) => to_hex(&h.finalize()),
            State::Sha256(h) => to_hex(&h.finalize()),
            State::Crc32(h) => format!("{:08x}", h.finalize()),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(algo: HashAlgo, chunks: &[&[u8]]) -> String {
        let mut h = StreamHasher::new(algo);
        chunks.iter().for_each(|c| h.update(c));
        h.finish()
    }

    #[test]
    fn known_vectors() {
        let fox: &[&[u8]] = &[b"The quick brown fox ", b"jumps over the lazy dog"];
        assert_eq!(hash(HashAlgo::Md5, &[]), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hash(HashAlgo::Md5, fox), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(hash(HashAlgo::Sha1, &[b"abc"]), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hash(HashAlgo::Sha1, fox), "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12");
        assert_eq!(
            hash(HashAlgo::Sha256, &[b"a", b"bc"]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash(HashAlgo::Crc32, &[b"123456789"]), "cbf43926");
        assert_eq!(hash(HashAlgo::Crc32, &[]), "00000000");
        assert_eq!(hash(HashAlgo::Crc32, fox), "414fa339");
    }
}
