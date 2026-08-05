//! Annex-B → avcC: the framing the browser actually wants.
//!
//! scrcpy emits Annex-B — NAL units separated by `00 00 01` start codes, with
//! the parameter sets in-band. `VideoDecoder` can be fed that, but the Annex-B
//! path re-converts every chunk and visibly tears under motion in Chrome; the
//! same finding drove the iOS side to hvcC (see
//! `stf-ios-provider/src/frontend/hevc-screen.js`). So the parameter sets are
//! lifted out into an `AVCDecoderConfigurationRecord` sent once, out of band,
//! and every frame is rewritten to 4-byte-length-prefixed NALUs.
//!
//! This is the H.264 mirror of what `backend-ios/src/hevc.rs` does for HEVC.
//! The two do not share code because the bitstreams differ where it matters —
//! one-byte vs two-byte NAL headers, and completely different configuration
//! records — and merging them would mean a branch in every function.

/// NAL unit types, from the low 5 bits of the header byte.
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|byte| byte & 0x1f).unwrap_or(0)
}

/// Split an Annex-B buffer into NAL units, dropping the start codes.
///
/// Handles both the 3-byte and 4-byte start codes, because a stream mixes
/// them: the 4-byte form is used before parameter sets and the first slice of
/// a picture, the 3-byte form elsewhere.
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }

    starts
        .iter()
        .enumerate()
        .filter_map(|(index, &(code_start, body_start))| {
            let end = starts
                .get(index + 1)
                .map(|&(next_code, _)| {
                    // A 4-byte start code is a 3-byte one preceded by a zero,
                    // which belongs to the *next* NAL, not this one's payload.
                    if next_code > 0 && data[next_code - 1] == 0 {
                        next_code - 1
                    } else {
                        next_code
                    }
                })
                .unwrap_or(data.len());
            let _ = code_start;
            (body_start < end).then(|| &data[body_start..end])
        })
        .collect()
}

/// Rewrite an Annex-B access unit as 4-byte-length-prefixed NALUs.
pub fn to_length_prefixed(data: &[u8]) -> Vec<u8> {
    let nals = split_annex_b(data);
    let total = nals.iter().map(|nal| nal.len() + 4).sum();
    let mut out = Vec::with_capacity(total);
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// The parameter sets carried by scrcpy's config packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

impl ParameterSets {
    /// Pull the SPS and PPS out of a config packet.
    pub fn parse(config: &[u8]) -> Option<Self> {
        let mut sps = None;
        let mut pps = None;
        for nal in split_annex_b(config) {
            match nal_type(nal) {
                NAL_SPS => sps = Some(nal.to_vec()),
                NAL_PPS => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        Some(Self {
            sps: sps?,
            pps: pps?,
        })
    }

    /// The WebCodecs codec string, `avc1.PPCCLL`.
    ///
    /// The three bytes are profile_idc, the constraint-set flags and level_idc,
    /// straight out of the SPS — exactly the bytes the string encodes, so this
    /// reads them rather than parsing the bitstream.
    pub fn codec_string(&self) -> Option<String> {
        let profile = self.sps.get(1)?;
        let constraints = self.sps.get(2)?;
        let level = self.sps.get(3)?;
        Some(format!("avc1.{profile:02X}{constraints:02X}{level:02X}"))
    }

    /// An ISO/IEC 14496-15 `AVCDecoderConfigurationRecord` (avcC).
    pub fn avcc(&self) -> Option<Vec<u8>> {
        let profile = *self.sps.get(1)?;
        let constraints = *self.sps.get(2)?;
        let level = *self.sps.get(3)?;

        let mut record = Vec::with_capacity(16 + self.sps.len() + self.pps.len());
        record.push(1); // configurationVersion
        record.push(profile);
        record.push(constraints);
        record.push(level);
        // 6 reserved bits set, then lengthSizeMinusOne = 3 (4-byte lengths).
        record.push(0xFF);
        // 3 reserved bits set, then numOfSequenceParameterSets = 1.
        record.push(0xE1);
        record.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
        record.extend_from_slice(&self.sps);
        record.push(1); // numOfPictureParameterSets
        record.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
        record.extend_from_slice(&self.pps);
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real config packet the probe read off a Galaxy S25+.
    const CONFIG: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x20, 0xac, 0xb4, 0x0f, 0x01, 0x03, 0xcb, 0xcd,
        0x41, 0x41, 0x81, 0x41, 0xb4, 0x28, 0x4d, 0x40, 0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x06,
        0xf2, 0xc0,
    ];

    #[test]
    fn splits_a_real_config_packet_into_sps_and_pps() {
        let nals = split_annex_b(CONFIG);
        assert_eq!(nals.len(), 2);
        assert_eq!(nal_type(nals[0]), NAL_SPS);
        assert_eq!(nal_type(nals[1]), NAL_PPS);
        // The trailing zero of the second start code must not be swallowed
        // into the first NAL's payload.
        assert_eq!(nals[0].last(), Some(&0x40));
    }

    #[test]
    fn builds_the_codec_string_and_avcc_a_browser_can_configure() {
        let sets = ParameterSets::parse(CONFIG).unwrap();
        // profile_idc 0x64 = High, level_idc 0x20 = 3.2.
        assert_eq!(sets.codec_string().unwrap(), "avc1.640020");

        let avcc = sets.avcc().unwrap();
        assert_eq!(avcc[0], 1, "configurationVersion");
        assert_eq!(&avcc[1..4], &[0x64, 0x00, 0x20]);
        assert_eq!(avcc[4], 0xFF, "4-byte NAL lengths");
        assert_eq!(avcc[5], 0xE1, "one SPS");
        let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
        assert_eq!(&avcc[8..8 + sps_len], sets.sps.as_slice());
        assert_eq!(avcc[8 + sps_len], 1, "one PPS");
    }

    #[test]
    fn rewrites_annex_b_as_length_prefixed_nalus() {
        let framed = to_length_prefixed(CONFIG);
        let sps_len = u32::from_be_bytes(framed[0..4].try_into().unwrap()) as usize;
        assert_eq!(sps_len, 19);
        assert_eq!(framed[4] & 0x1f, NAL_SPS);

        let pps_offset = 4 + sps_len;
        let pps_len = u32::from_be_bytes(framed[pps_offset..pps_offset + 4].try_into().unwrap());
        assert_eq!(pps_len, 5, "68 ee 06 f2 c0");
        assert_eq!(framed.len(), 4 + sps_len + 4 + pps_len as usize);
    }

    #[test]
    fn handles_three_byte_start_codes() {
        let data = [0x00, 0x00, 0x01, 0x65, 0xAA, 0x00, 0x00, 0x01, 0x41, 0xBB];
        let nals = split_annex_b(&data);
        assert_eq!(nals, vec![&[0x65u8, 0xAA][..], &[0x41, 0xBB][..]]);
    }

    #[test]
    fn a_config_packet_missing_a_set_yields_nothing_rather_than_half_a_record() {
        // SPS only: configuring a decoder without the PPS produces a decoder
        // that accepts the config and then fails on the first frame.
        let sps_only = &CONFIG[..23];
        assert!(ParameterSets::parse(sps_only).is_none());
    }
}
