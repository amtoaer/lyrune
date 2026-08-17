// Derived from jixunmoe-go/qrc's MIT-licensed QRC DES implementation.

const KEY_1: &[u8; 8] = b"!@#)(*$%";
const KEY_2: &[u8; 8] = b"123ZXC!@";
const KEY_3: &[u8; 8] = b"!@#)(NHL";

pub(super) fn decrypt_in_place(data: &mut [u8]) -> bool {
    transform_in_place(data, false)
}

#[cfg(test)]
pub(super) fn encrypt_in_place(data: &mut [u8]) -> bool {
    transform_in_place(data, true)
}

fn transform_in_place(data: &mut [u8], encrypt: bool) -> bool {
    if data.len() % 8 != 0 {
        return false;
    }

    let stages = if encrypt {
        [
            QrcDes::new(KEY_1, true),
            QrcDes::new(KEY_2, false),
            QrcDes::new(KEY_3, true),
        ]
    } else {
        [
            QrcDes::new(KEY_3, false),
            QrcDes::new(KEY_2, true),
            QrcDes::new(KEY_1, false),
        ]
    };
    for stage in stages {
        stage.transform_bytes(data);
    }
    true
}

struct QrcDes {
    subkeys: [u64; 16],
}

impl QrcDes {
    fn new(key: &[u8; 8], encrypt: bool) -> Self {
        let key = u64::from_le_bytes(*key);
        let permuted = map_u64(key, &KEY_PERMUTATION);
        let mut c = permuted as u32;
        let mut d = (permuted >> 32) as u32;
        let mut subkeys = [0; 16];

        for (index, shift) in KEY_SHIFTS.into_iter().enumerate() {
            update_key_half(&mut c, shift);
            update_key_half(&mut d, shift);
            let subkey_index = if encrypt { index } else { 15 - index };
            subkeys[subkey_index] = map_u64(make_u64(d, c), &KEY_COMPRESSION);
        }
        Self { subkeys }
    }

    fn transform_bytes(&self, data: &mut [u8]) {
        for block in data.chunks_exact_mut(8) {
            let input = u64::from_le_bytes(block.try_into().expect("DES block has eight bytes"));
            block.copy_from_slice(&self.transform_block(input).to_le_bytes());
        }
    }

    fn transform_block(&self, data: u64) -> u64 {
        let mut state = map_u64(data, &INITIAL_PERMUTATION);
        for key in self.subkeys {
            state = crypt_round(state, key);
        }
        map_u64(state.rotate_left(32), &INVERSE_PERMUTATION)
    }
}

fn update_key_half(value: &mut u32, shift: u8) {
    *value = (*value << shift) | ((*value >> (28 - shift)) & 0xffff_fff0);
}

fn crypt_round(state: u64, key: u64) -> u64 {
    let high = (state >> 32) as u32;
    let low = state as u32;
    let expanded = map_u64(make_u64(high, high), &KEY_EXPANSION) ^ key;
    let substituted = SBOX_SHIFTS
        .into_iter()
        .enumerate()
        .fold(0_u32, |result, (index, shift)| {
            let sbox_index = ((expanded >> shift) & 0b11_1111) as usize;
            (result << 4) | u32::from(SBOXES[index][sbox_index])
        });
    let next_low = map_u32(substituted, &P_BOX) ^ low;
    make_u64(next_low, high)
}

fn make_u64(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn map_u32(source: u32, table: &[u8]) -> u32 {
    let mut result = 0_u64;
    for (index, source_index) in table.iter().copied().enumerate() {
        map_bit(&mut result, u64::from(source), source_index, index as u8);
    }
    result as u32
}

fn map_u64(source: u64, table: &[u8]) -> u64 {
    let middle = table.len() / 2;
    let mut low = 0_u64;
    let mut high = 0_u64;
    for (index, source_index) in table[..middle].iter().copied().enumerate() {
        map_bit(&mut low, source, source_index, index as u8);
    }
    for (index, source_index) in table[middle..].iter().copied().enumerate() {
        map_bit(&mut high, source, source_index, index as u8);
    }
    make_u64(high as u32, low as u32)
}

fn map_bit(result: &mut u64, source: u64, source_index: u8, target_index: u8) {
    if bit_mask(source_index) & source != 0 {
        *result |= bit_mask(target_index);
    }
}

fn bit_mask(index: u8) -> u64 {
    if index < 32 {
        1_u64 << (31 - index)
    } else {
        1_u64 << (95 - index)
    }
}

const KEY_SHIFTS: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];
const SBOX_SHIFTS: [u8; 8] = [26, 20, 14, 8, 58, 52, 46, 40];

const SBOXES: [[u8; 64]; 8] = [
    [
        14, 0, 4, 15, 13, 7, 1, 4, 2, 14, 15, 2, 11, 13, 8, 1, 3, 10, 10, 6, 6, 12, 12, 11, 5, 9,
        9, 5, 0, 3, 7, 8, 4, 15, 1, 12, 14, 8, 8, 2, 13, 4, 6, 9, 2, 1, 11, 7, 15, 5, 12, 11, 9, 3,
        7, 14, 3, 10, 10, 0, 5, 6, 0, 13,
    ],
    [
        15, 3, 1, 13, 8, 4, 14, 7, 6, 15, 11, 2, 3, 8, 4, 15, 9, 12, 7, 0, 2, 1, 13, 10, 12, 6, 0,
        9, 5, 11, 10, 5, 0, 13, 14, 8, 7, 10, 11, 1, 10, 3, 4, 15, 13, 4, 1, 2, 5, 11, 8, 6, 12, 7,
        6, 12, 9, 0, 3, 5, 2, 14, 15, 9,
    ],
    [
        10, 13, 0, 7, 9, 0, 14, 9, 6, 3, 3, 4, 15, 6, 5, 10, 1, 2, 13, 8, 12, 5, 7, 14, 11, 12, 4,
        11, 2, 15, 8, 1, 13, 1, 6, 10, 4, 13, 9, 0, 8, 6, 15, 9, 3, 8, 0, 7, 11, 4, 1, 15, 2, 14,
        12, 3, 5, 11, 10, 5, 14, 2, 7, 12,
    ],
    [
        7, 13, 13, 8, 14, 11, 3, 5, 0, 6, 6, 15, 9, 0, 10, 3, 1, 4, 2, 7, 8, 2, 5, 12, 11, 1, 12,
        10, 4, 14, 15, 9, 10, 3, 6, 15, 9, 0, 0, 6, 12, 10, 11, 10, 7, 13, 13, 8, 15, 9, 1, 4, 3,
        5, 14, 11, 5, 12, 2, 7, 8, 2, 4, 14,
    ],
    [
        2, 14, 12, 11, 4, 2, 1, 12, 7, 4, 10, 7, 11, 13, 6, 1, 8, 5, 5, 0, 3, 15, 15, 10, 13, 3, 0,
        9, 14, 8, 9, 6, 4, 11, 2, 8, 1, 12, 11, 7, 10, 1, 13, 14, 7, 2, 8, 13, 15, 6, 9, 15, 12, 0,
        5, 9, 6, 10, 3, 4, 0, 5, 14, 3,
    ],
    [
        12, 10, 1, 15, 10, 4, 15, 2, 9, 7, 2, 12, 6, 9, 8, 5, 0, 6, 13, 1, 3, 13, 4, 14, 14, 0, 7,
        11, 5, 3, 11, 8, 9, 4, 14, 3, 15, 2, 5, 12, 2, 9, 8, 5, 12, 15, 3, 10, 7, 11, 0, 14, 4, 1,
        10, 7, 1, 6, 13, 0, 11, 8, 6, 13,
    ],
    [
        4, 13, 11, 0, 2, 11, 14, 7, 15, 4, 0, 9, 8, 1, 13, 10, 3, 14, 12, 3, 9, 5, 7, 12, 5, 2, 10,
        15, 6, 8, 1, 6, 1, 6, 4, 11, 11, 13, 13, 8, 12, 1, 3, 4, 7, 10, 14, 7, 10, 9, 15, 5, 6, 0,
        8, 15, 0, 14, 5, 2, 9, 3, 2, 12,
    ],
    [
        13, 1, 2, 15, 8, 13, 4, 8, 6, 10, 15, 3, 11, 7, 1, 4, 10, 12, 9, 5, 3, 6, 14, 11, 5, 0, 0,
        14, 12, 9, 7, 2, 7, 2, 11, 1, 4, 14, 1, 7, 9, 4, 12, 10, 14, 8, 2, 13, 0, 15, 6, 12, 10, 9,
        13, 0, 15, 3, 3, 5, 5, 6, 8, 11,
    ],
];

const P_BOX: [u8; 32] = [
    15, 6, 19, 20, 28, 11, 27, 16, 0, 14, 22, 25, 4, 17, 30, 9, 1, 7, 23, 13, 31, 26, 2, 8, 18, 12,
    29, 5, 21, 10, 3, 24,
];
const INITIAL_PERMUTATION: [u8; 64] = [
    57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61, 53, 45, 37, 29, 21, 13, 5, 63,
    55, 47, 39, 31, 23, 15, 7, 56, 48, 40, 32, 24, 16, 8, 0, 58, 50, 42, 34, 26, 18, 10, 2, 60, 52,
    44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
];
const INVERSE_PERMUTATION: [u8; 64] = [
    39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30, 37, 5, 45, 13, 53, 21, 61, 29,
    36, 4, 44, 12, 52, 20, 60, 28, 35, 3, 43, 11, 51, 19, 59, 27, 34, 2, 42, 10, 50, 18, 58, 26,
    33, 1, 41, 9, 49, 17, 57, 25, 32, 0, 40, 8, 48, 16, 56, 24,
];
const KEY_PERMUTATION: [u8; 56] = [
    56, 48, 40, 32, 24, 16, 8, 0, 57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59,
    51, 43, 35, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29, 21, 13, 5, 60, 52, 44, 36, 28,
    20, 12, 4, 27, 19, 11, 3,
];
const KEY_COMPRESSION: [u8; 48] = [
    13, 16, 10, 23, 0, 4, 2, 27, 14, 5, 20, 9, 22, 18, 11, 3, 25, 7, 15, 6, 26, 19, 12, 1, 45, 56,
    35, 41, 51, 59, 34, 44, 55, 49, 37, 52, 48, 53, 43, 60, 38, 57, 50, 46, 54, 40, 33, 36,
];
const KEY_EXPANSION: [u8; 48] = [
    31, 0, 1, 2, 3, 4, 3, 4, 5, 6, 7, 8, 7, 8, 9, 10, 11, 12, 11, 12, 13, 14, 15, 16, 15, 16, 17,
    18, 19, 20, 19, 20, 21, 22, 23, 24, 23, 24, 25, 26, 27, 28, 27, 28, 29, 30, 31, 0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_upstream_decryption_vector() {
        let mut block = hex::decode("0123456789abcdef").unwrap();
        assert!(decrypt_in_place(&mut block));
        assert_eq!(hex::encode(block), "5a92c2297192224e");
    }
}
