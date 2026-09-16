use std::io::{self, Read, Seek, SeekFrom};

use base64::Engine as _;

const FOOTER_SIZE: usize = 0xc0;
const MAGIC: &[u8; 8] = b"musicex\0";
const ENC_V2_PREFIX: &[u8] = b"QQMusic EncV2,Key:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicEx {
    pub audio_len: u64,
    pub song_id: u32,
    pub media_mid: String,
    pub resource_filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidEkey,
    InvalidFooter(&'static str),
}

pub fn parse_musicex(data: &[u8]) -> Result<MusicEx, Error> {
    if data.len() < FOOTER_SIZE {
        return Err(Error::InvalidFooter("truncated"));
    }
    parse_musicex_footer(
        &data[data.len() - FOOTER_SIZE..],
        (data.len() - FOOTER_SIZE) as u64,
    )
}

pub fn parse_musicex_footer(footer: &[u8], audio_len: u64) -> Result<MusicEx, Error> {
    if footer.len() != FOOTER_SIZE {
        return Err(Error::InvalidFooter("invalid size"));
    }
    let size = u32::from_le_bytes(footer[0xb0..0xb4].try_into().unwrap());
    let version = u32::from_le_bytes(footer[0xb4..0xb8].try_into().unwrap());
    if size as usize != FOOTER_SIZE || version != 1 || &footer[0xb8..] != MAGIC {
        return Err(Error::InvalidFooter("invalid trailer"));
    }
    let media_mid = decode_utf16(&footer[0x0c..0x48])?;
    let resource_filename = decode_utf16(&footer[0x48..0x8c])?;
    if media_mid.is_empty() {
        return Err(Error::InvalidFooter("empty media mid"));
    }
    if resource_filename.is_empty()
        || resource_filename.contains(['/', '\\'])
        || (!resource_filename.ends_with(".mflac") && !resource_filename.ends_with(".mgg"))
    {
        return Err(Error::InvalidFooter("invalid resource filename"));
    }
    Ok(MusicEx {
        audio_len,
        song_id: u32::from_le_bytes(footer[..4].try_into().unwrap()),
        media_mid,
        resource_filename,
    })
}

fn decode_utf16(field: &[u8]) -> Result<String, Error> {
    let units = field
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .ok_or(Error::InvalidFooter("unterminated utf16"))?;
    if units[end + 1..].iter().any(|unit| *unit != 0) {
        return Err(Error::InvalidFooter("nonzero utf16 suffix"));
    }
    String::from_utf16(&units[..end]).map_err(|_| Error::InvalidFooter("invalid utf16"))
}

pub fn derive_ekey(ekey: &str) -> Result<Vec<u8>, Error> {
    let mut decoded = base64::engine::general_purpose::STANDARD
        .decode(ekey)
        .map_err(|_| Error::InvalidEkey)?;
    if decoded.starts_with(ENC_V2_PREFIX) {
        decoded = derive_v2(&decoded[ENC_V2_PREFIX.len()..])?;
    }
    derive_v1(&decoded)
}

fn derive_v1(raw: &[u8]) -> Result<Vec<u8>, Error> {
    if raw.len() < 16 {
        return Err(Error::InvalidEkey);
    }
    let simple = simple_key(106, 8);
    let mut tea_key = [0u8; 16];
    for index in 0..8 {
        tea_key[index * 2] = simple[index];
        tea_key[index * 2 + 1] = raw[index];
    }
    let mut result = raw[..8].to_vec();
    result.extend(decrypt_tea(&raw[8..], &tea_key)?);
    Ok(result)
}

fn derive_v2(raw: &[u8]) -> Result<Vec<u8>, Error> {
    let first = decrypt_tea(raw, b"386ZJY!@#*$%^&)(")?;
    let second = decrypt_tea(&first, b"**#!(#$%&^a1cZ,T")?;
    base64::engine::general_purpose::STANDARD
        .decode(second)
        .map_err(|_| Error::InvalidEkey)
}

fn simple_key(salt: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| ((f64::from(salt) + index as f64 * 0.1).tan().abs() * 100.) as u8)
        .collect()
}

fn decrypt_tea(input: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, Error> {
    const SALT: usize = 2;
    const ZERO: usize = 7;
    if input.len() < 16 || input.len() % 8 != 0 {
        return Err(Error::InvalidEkey);
    }
    let mut block = [0u8; 8];
    block.copy_from_slice(&input[..8]);
    tea_decrypt(&mut block, key);
    let padding = usize::from(block[0] & 7);
    let length = input
        .len()
        .checked_sub(1 + padding + SALT + ZERO)
        .ok_or(Error::InvalidEkey)?;
    let mut output = vec![0; length];
    let mut previous = [0u8; 8];
    let mut current = [0u8; 8];
    current.copy_from_slice(&input[..8]);
    let mut input_pos = 8;
    let mut block_pos = 1 + padding;
    let mut output_pos = 0;
    macro_rules! next_block {
        () => {{
            if input_pos + 8 > input.len() {
                return Err(Error::InvalidEkey);
            }
            previous = current;
            current.copy_from_slice(&input[input_pos..input_pos + 8]);
            for index in 0..8 {
                block[index] ^= current[index];
            }
            tea_decrypt(&mut block, key);
            input_pos += 8;
            block_pos = 0;
        }};
    }
    let mut skipped = 0;
    while skipped < SALT {
        if block_pos < 8 {
            block_pos += 1;
            skipped += 1;
        } else {
            next_block!();
        }
    }
    while output_pos < length {
        if block_pos < 8 {
            output[output_pos] = block[block_pos] ^ previous[block_pos];
            block_pos += 1;
            output_pos += 1;
        } else {
            next_block!();
        }
    }
    for _ in 0..ZERO {
        if block_pos >= 8 || block[block_pos] != previous[block_pos] {
            return Err(Error::InvalidEkey);
        }
        block_pos += 1;
    }
    Ok(output)
}

fn tea_decrypt(block: &mut [u8; 8], key: &[u8; 16]) {
    let k = |index| u32::from_be_bytes(key[index..index + 4].try_into().unwrap());
    let (mut v0, mut v1) = (
        u32::from_be_bytes(block[..4].try_into().unwrap()),
        u32::from_be_bytes(block[4..].try_into().unwrap()),
    );
    let delta = 0x9e3779b9u32;
    let mut sum = delta.wrapping_mul(16);
    for _ in 0..16 {
        v1 = v1.wrapping_sub(
            ((v0 << 4).wrapping_add(k(8))) ^ v0.wrapping_add(sum) ^ (v0 >> 5).wrapping_add(k(12)),
        );
        v0 = v0.wrapping_sub(
            ((v1 << 4).wrapping_add(k(0))) ^ v1.wrapping_add(sum) ^ (v1 >> 5).wrapping_add(k(4)),
        );
        sum = sum.wrapping_sub(delta);
    }
    block[..4].copy_from_slice(&v0.to_be_bytes());
    block[4..].copy_from_slice(&v1.to_be_bytes());
}

pub struct QmcCipher {
    inner: Cipher,
}

enum Cipher {
    Map(Vec<u8>),
    Rc4(Rc4),
}

impl QmcCipher {
    pub fn from_ekey(ekey: &str) -> Result<Self, Error> {
        let key = derive_ekey(ekey)?;
        Ok(Self {
            inner: if key.len() > 300 {
                Cipher::Rc4(Rc4::new(key))
            } else {
                Cipher::Map(
                    (0..0x8000)
                        .map(|offset| {
                            let index = (offset * offset + 71214) % key.len();
                            let value = key[index];
                            let shift = ((index as u8 & 7) + 4) % 8;
                            (value << shift) | (value >> shift)
                        })
                        .collect(),
                )
            },
        })
    }

    pub fn decrypt(&mut self, data: &mut [u8], offset: u64) {
        match &mut self.inner {
            Cipher::Map(masks) => {
                for (index, byte) in data.iter_mut().enumerate() {
                    let position = offset + index as u64;
                    let position = if position > 0x7fff {
                        position % 0x7fff
                    } else {
                        position
                    };
                    *byte ^= masks[position as usize];
                }
            }
            Cipher::Rc4(rc4) => rc4.decrypt(data, offset),
        }
    }
}

struct Rc4 {
    key: Vec<u8>,
    state: Vec<u8>,
    hash: u32,
}

impl Rc4 {
    fn new(key: Vec<u8>) -> Self {
        let mut state = (0..key.len()).map(|index| index as u8).collect::<Vec<_>>();
        let mut j = 0;
        for index in 0..key.len() {
            j = (j + usize::from(state[index]) + usize::from(key[index])) % key.len();
            state.swap(index, j);
        }
        let mut hash = 1u32;
        for &value in &key {
            if value != 0 {
                let next = hash.wrapping_mul(u32::from(value));
                if next == 0 || next <= hash {
                    break;
                }
                hash = next;
            }
        }
        Self { key, state, hash }
    }

    fn skip(&self, id: usize) -> usize {
        let seed = usize::from(self.key[id % self.key.len()]);
        if seed == 0 {
            return 0;
        }
        (self.hash as f64 / ((id + 1) * seed) as f64 * 100.) as usize % self.key.len()
    }

    fn decrypt(&self, mut data: &mut [u8], offset: u64) {
        let mut position = offset as usize;
        if position < 128 {
            let length = data.len().min(128 - position);
            for index in 0..length {
                data[index] ^= self.key[self.skip(position + index)];
            }
            data = &mut data[length..];
            position += length;
        }
        let mut processed = 0;
        let mut work = vec![0u8; self.state.len()];
        while processed < data.len() {
            let length = (data.len() - processed).min(5120 - position % 5120);
            work.copy_from_slice(&self.state);
            let mut j = 0;
            let mut k = 0;
            let skip = position % 5120 + self.skip(position / 5120);
            for index in -(skip as isize)..length as isize {
                j = (j + 1) % work.len();
                k = (usize::from(work[j]) + k) % work.len();
                work.swap(j, k);
                if index >= 0 {
                    data[processed + index as usize] ^=
                        work[(usize::from(work[j]) + usize::from(work[k])) % work.len()];
                }
            }
            processed += length;
            position += length;
        }
    }
}

pub struct DecryptReader<R> {
    inner: R,
    cipher: QmcCipher,
    position: u64,
    length: u64,
}

impl<R> DecryptReader<R> {
    pub fn new(inner: R, cipher: QmcCipher, length: u64) -> Self {
        Self {
            inner,
            cipher,
            position: 0,
            length,
        }
    }
}

impl<R: Read> Read for DecryptReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let limit = buffer
            .len()
            .min(self.length.saturating_sub(self.position) as usize);
        if limit == 0 {
            return Ok(0);
        }
        let read = self.inner.read(&mut buffer[..limit])?;
        self.cipher.decrypt(&mut buffer[..read], self.position);
        self.position += read as u64;
        Ok(read)
    }
}

impl<R: Read + Seek> Seek for DecryptReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(value) => value,
            SeekFrom::Current(value) => self.position.saturating_add_signed(value),
            SeekFrom::End(value) => self.length.saturating_add_signed(value),
        }
        .min(self.length);
        self.inner.seek(SeekFrom::Start(target))?;
        self.position = target;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer(media_mid: &str, filename: &str) -> Vec<u8> {
        let mut footer = vec![0u8; FOOTER_SIZE];
        footer[0..4].copy_from_slice(&123u32.to_le_bytes());
        footer[0xb0..0xb4].copy_from_slice(&(FOOTER_SIZE as u32).to_le_bytes());
        footer[0xb4..0xb8].copy_from_slice(&1u32.to_le_bytes());
        for (offset, value) in [(0x0c, media_mid), (0x48, filename)] {
            for (index, unit) in value.encode_utf16().chain(std::iter::once(0)).enumerate() {
                footer[offset + index * 2..offset + index * 2 + 2]
                    .copy_from_slice(&unit.to_le_bytes());
            }
        }
        footer[0xb8..].copy_from_slice(MAGIC);
        footer
    }

    #[test]
    fn parses_musicex_footer_and_audio_length() {
        let footer = footer("media", "F0M0media.mflac");
        let parsed = parse_musicex_footer(&footer, 42).unwrap();
        assert_eq!(parsed.audio_len, 42);
        assert_eq!(parsed.media_mid, "media");
        assert_eq!(parsed.resource_filename, "F0M0media.mflac");
    }

    #[test]
    fn rejects_invalid_musicex_filename() {
        let footer = footer("media", "../bad.mflac");
        assert_eq!(
            parse_musicex_footer(&footer, 42),
            Err(Error::InvalidFooter("invalid resource filename"))
        );
    }
}
