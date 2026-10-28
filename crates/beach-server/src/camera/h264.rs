//! Just enough H.264 bitstream handling to move Annex B access units between
//! the encoders and decoders of the platforms.

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

/// The NAL units of an Annex B byte stream, without their start codes.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        // A four-byte start code leaves its leading zero on the previous unit.
        while end > start && data[end - 1] == 0 && n + 1 < starts.len() {
            end -= 1;
        }
        if end > start {
            units.push(&data[start..end]);
        }
    }
    units
}

pub fn nal_type(unit: &[u8]) -> u8 {
    unit.first().map_or(0, |b| b & 0x1f)
}

/// Whether the access unit can start a stream: it has an IDR slice.
pub fn is_key_frame(data: &[u8]) -> bool {
    nal_units(data).iter().any(|u| nal_type(u) == NAL_IDR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let stream = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 0, 5];
        let units = nal_units(&stream);
        assert_eq!(units, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4, 0, 5]]);
        assert_eq!(units.iter().map(|u| nal_type(u)).collect::<Vec<_>>(), [NAL_SPS, NAL_PPS, NAL_IDR]);
        assert!(is_key_frame(&stream));
        assert!(!is_key_frame(&[0, 0, 1, 0x41, 9]));
        assert!(nal_units(&[1, 2, 3]).is_empty());
        assert!(nal_units(&[0, 0, 1]).is_empty());
    }
}
