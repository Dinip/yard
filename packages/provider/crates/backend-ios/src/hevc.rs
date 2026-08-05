//! HEVC depacketisation and the browser-facing access-unit format.
//!
//! Ported from `stf-ios-provider/src/device/hevc.rs`. Only the framing byte and
//! the `AccessUnit` shape were dropped: `farm-protocol` owns the wire framing
//! now, and `provider-core::video` owns the access-unit type, because both are
//! shared with the Android backend.
//!
//! `idevice` ships an `HevcDepacketizer`, but it emits a continuous Annex-B
//! stream, keeps its parameter sets private, and does not strip Apple's
//! proprietary NAL trailer (see [`DISPLAYSERVICE_NAL_TRAILER`]). We need NAL-level
//! access for all three, so RFC 7798 reassembly lives here instead — it is
//! about sixty lines, and owning it also lets access units be cut on the RTP
//! marker bit rather than inferred from a timestamp change.
//!
//! What goes to the browser is the hvcC/ISO-14496-15 sample format:
//! 4-byte-length-prefixed NALUs, with the parameter sets supplied out of band
//! as a `HEVCDecoderConfigurationRecord`. That routes Chrome's `VideoDecoder`
//! through VideoToolbox's native hvcC path; the Annex-B start-code path
//! re-converts every chunk and visibly tears under rapid motion.

pub const NAL_IDR_W_RADL: u8 = 19;
pub const NAL_IDR_N_LP: u8 = 20;
pub const NAL_CRA: u8 = 21;
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
/// RFC 7798 aggregation packet.
const NAL_AP: u8 = 48;
/// RFC 7798 fragmentation unit.
const NAL_FU: u8 = 49;

/// Apple's DisplayService appends this fixed 14-byte proprietary footer after
/// the final fragment of each coded-slice NAL. It is not part of the HEVC
/// bitstream — avconferenced (the receiver DeviceHub and Xcode use) strips it
/// before decode, whereas a plain RFC 7798 reassembly would feed it to the
/// decoder as trailing slice data. Matched as an exact suffix, so this is a
/// no-op on streams that do not carry it.
const DISPLAYSERVICE_NAL_TRAILER: &[u8] = &[
    0x04, 0xf0, 0x0a, 0xc0, 0x00, 0x00, 0x03, 0x00, 0x00, 0x04, 0xec, 0x0a, 0xb0, 0x03,
];

pub fn nal_type(nal: &[u8]) -> u8 {
    (nal[0] >> 1) & 0x3f
}

pub fn is_key_nal(nal_type: u8) -> bool {
    matches!(nal_type, NAL_IDR_W_RADL | NAL_IDR_N_LP | NAL_CRA)
}

fn strip_trailer(nal: &[u8]) -> &[u8] {
    if nal.ends_with(DISPLAYSERVICE_NAL_TRAILER) {
        &nal[..nal.len() - DISPLAYSERVICE_NAL_TRAILER.len()]
    } else {
        nal
    }
}

/// Reassembles NAL units from RTP/HEVC payloads (RFC 7798).
#[derive(Default)]
pub struct Depacketizer {
    fu_buffer: Vec<u8>,
    /// Whether a start fragment has been seen for the NAL being assembled.
    /// Without it a continuation that arrives after a reset would be emitted as
    /// a NAL with no header at all.
    fu_active: bool,
}

impl Depacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discard any half-assembled fragment.
    ///
    /// Called on an RTP sequence gap: stitching non-contiguous payloads into
    /// one NAL produces a NAL that is structurally valid and semantically
    /// garbage, which is far worse than dropping it.
    pub fn reset_fragment(&mut self) {
        self.fu_buffer.clear();
        self.fu_active = false;
    }

    /// Process one RTP payload, appending every complete NAL unit to `out`.
    pub fn push(&mut self, payload: &[u8], out: &mut Vec<Vec<u8>>) {
        if payload.len() < 2 {
            return;
        }

        match nal_type(payload) {
            NAL_AP => {
                let mut i = 2;
                while i + 2 <= payload.len() {
                    let size = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
                    i += 2;
                    let end = (i + size).min(payload.len());
                    if i < end {
                        out.push(strip_trailer(&payload[i..end]).to_vec());
                    }
                    i = end;
                }
            }
            NAL_FU => {
                if payload.len() < 3 {
                    return;
                }
                let fu_header = payload[2];
                let start = fu_header & 0x80 != 0;
                let end = fu_header & 0x40 != 0;
                let original_nal_type = fu_header & 0x3f;

                if start {
                    // Rebuild the original two-byte NAL header: keep the
                    // forbidden-zero and layer-id bits from the payload header,
                    // splice the real type back in from the FU header.
                    self.fu_buffer.clear();
                    self.fu_buffer
                        .push((payload[0] & 0x81) | (original_nal_type << 1));
                    self.fu_buffer.push(payload[1]);
                    self.fu_active = true;
                } else if !self.fu_active {
                    // A continuation whose start fragment we never saw. Its
                    // bytes carry no NAL header, so emitting them would hand
                    // the decoder a headerless unit.
                    return;
                }
                self.fu_buffer.extend_from_slice(&payload[3..]);

                if end {
                    out.push(strip_trailer(&self.fu_buffer).to_vec());
                    self.reset_fragment();
                }
            }
            _ => out.push(strip_trailer(payload).to_vec()),
        }
    }
}

/// The parameter sets needed to describe the stream to a decoder.
#[derive(Default, Clone)]
pub struct ParameterSets {
    pub vps: Option<Vec<u8>>,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
    pub codec_string: Option<String>,
    /// Encoded frame size, which is the only display geometry available on
    /// iOS versions where mobilegestalt is deprecated.
    pub dimensions: Option<(i64, i64)>,
}

impl ParameterSets {
    /// Record a NAL if it is a parameter set. Returns true if it was consumed.
    pub fn observe(&mut self, nal: &[u8]) -> bool {
        match nal_type(nal) {
            NAL_VPS => self.vps = Some(nal.to_vec()),
            NAL_PPS => self.pps = Some(nal.to_vec()),
            NAL_SPS => {
                self.sps = Some(nal.to_vec());
                if self.dimensions.is_none() {
                    self.dimensions = dimensions_from_sps(nal);
                }
                if self.codec_string.is_none() {
                    match codec_string_from_sps(nal) {
                        Ok(codec) => {
                            tracing::info!(codec, "WebCodecs codec string");
                            self.codec_string = Some(codec);
                        }
                        Err(err) => tracing::warn!(%err, "failed to parse SPS"),
                    }
                }
            }
            _ => return false,
        }
        true
    }

    pub fn is_complete(&self) -> bool {
        self.vps.is_some()
            && self.sps.is_some()
            && self.pps.is_some()
            && self.codec_string.is_some()
    }

    /// `(codec string, hvcC record)` once every parameter set has been seen.
    pub fn description(&self) -> Option<(String, Vec<u8>)> {
        let (vps, sps, pps, codec) = (
            self.vps.as_ref()?,
            self.sps.as_ref()?,
            self.pps.as_ref()?,
            self.codec_string.as_ref()?,
        );
        Some((
            codec.clone(),
            decoder_configuration_record(vps, sps, pps).ok()?,
        ))
    }
}

/// Strip the 2-byte NAL header and emulation-prevention bytes (`00 00 03` →
/// `00 00`).
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len().saturating_sub(2));
    let mut i = 2;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            out.extend_from_slice(&nal[i..i + 2]);
            i += 3;
        } else {
            out.push(nal[i]);
            i += 1;
        }
    }
    out
}

/// Parse the SPS and return the WebCodecs codec string.
///
/// Encoded frame size from the SPS, in luma samples.
///
/// This is where display geometry comes from on modern iOS. The lockdown
/// `mobilegestalt` query that `stf-ios-provider` used answers
/// `MobileGestaltDeprecated` on iOS 26/27 and returns no screen dimensions at
/// all, so it cannot be relied on. The SPS is better anyway: it reports what is
/// actually being encoded, which is what a viewer sees, rather than what the
/// panel measures.
///
/// Not carried over from the reference implementation — it did not need this,
/// because on the iOS versions it targeted mobilegestalt still answered.
pub fn dimensions_from_sps(sps: &[u8]) -> Option<(i64, i64)> {
    let rb = rbsp(sps);
    let mut reader = BitReader::new(&rb);

    reader.bits(4); // sps_video_parameter_set_id
    let max_sub_layers_minus1 = reader.bits(3) as u32;
    reader.bits(1); // sps_temporal_id_nesting_flag

    // profile_tier_level: the general block, then the sub-layer blocks. Apple's
    // encoder emits a single layer, but skipping the loop would silently
    // mis-parse anything that does not.
    reader.bits(2 + 1 + 5); // profile_space, tier_flag, profile_idc
    reader.bits(32); // general_profile_compatibility_flags
    reader.bits(48); // general_constraint_indicator_flags
    reader.bits(8); // general_level_idc

    let mut sub_layer_profile = Vec::new();
    let mut sub_layer_level = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        sub_layer_profile.push(reader.bits(1) != 0);
        sub_layer_level.push(reader.bits(1) != 0);
    }
    if max_sub_layers_minus1 > 0 {
        // reserved_zero_2bits, padded out to eight entries.
        for _ in max_sub_layers_minus1..8 {
            reader.bits(2);
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if sub_layer_profile[i] {
            reader.bits(2 + 1 + 5 + 32 + 48);
        }
        if sub_layer_level[i] {
            reader.bits(8);
        }
    }

    reader.ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc = reader.ue()?;
    if chroma_format_idc == 3 {
        reader.bits(1); // separate_colour_plane_flag
    }

    let width = reader.ue()? as i64;
    let height = reader.ue()? as i64;
    if reader.overran() || width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// Just enough of a bitstream reader for the SPS fields above.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bits(&mut self, count: u32) -> u64 {
        let mut value = 0u64;
        for _ in 0..count {
            let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            value = (value << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        value
    }

    /// Exp-Golomb, the variable-length coding every SPS field above uses.
    fn ue(&mut self) -> Option<u64> {
        let mut leading = 0u32;
        while self.bits(1) == 0 {
            leading += 1;
            // A run this long means we are reading past the data as zeroes
            // rather than parsing a real value.
            if leading > 32 || self.overran() {
                return None;
            }
        }
        Some((1u64 << leading) - 1 + self.bits(leading))
    }

    fn overran(&self) -> bool {
        self.pos > self.data.len() * 8
    }
}

/// Format per ISO/IEC 14496-15 §A.3.3.1:
/// `hev1.<profile_space><profile_idc>.<reversed_pcf>.<tier><level>.<constraints>`
pub fn codec_string_from_sps(sps: &[u8]) -> Result<String, &'static str> {
    let rb = rbsp(sps);

    let mut pos = 0usize;
    let mut read_bits = |n: u32| -> u64 {
        let mut value = 0u64;
        for _ in 0..n {
            let byte = rb.get(pos >> 3).copied().unwrap_or(0);
            value = (value << 1) | ((byte >> (7 - (pos & 7))) & 1) as u64;
            pos += 1;
        }
        value
    };

    read_bits(4); // sps_video_parameter_set_id
    read_bits(3); // sps_max_sub_layers_minus1
    read_bits(1); // sps_temporal_id_nesting_flag
    let profile_space = read_bits(2) as usize;
    let tier_flag = read_bits(1);
    let profile_idc = read_bits(5);
    let pcf = read_bits(32) as u32;
    let cif = read_bits(48);
    let level_idc = read_bits(8);

    if pos > rb.len() * 8 {
        return Err("SPS too short to contain profile_tier_level");
    }

    let profile_space_char = match profile_space {
        0 => "",
        1 => "A",
        2 => "B",
        3 => "C",
        _ => "D",
    };
    let tier_char = if tier_flag != 0 { 'H' } else { 'L' };

    let mut constraints = format!("{cif:012X}");
    while constraints.len() > 2 && constraints.ends_with("00") {
        constraints.truncate(constraints.len() - 2);
    }

    Ok(format!(
        "hev1.{profile_space_char}{profile_idc}.{:X}.{tier_char}{level_idc}.{constraints}",
        pcf.reverse_bits()
    ))
}

/// Build an ISO/IEC 14496-15 §8.3.3.1 `HEVCDecoderConfigurationRecord` (hvcC).
///
/// The 12-byte general `profile_tier_level` lives at RBSP bytes 1..13 of the
/// SPS, right after the 1-byte vps_id/max_sub_layers/nesting field, with the
/// exact byte layout hvcC wants — so it is copied verbatim rather than
/// re-serialised from the parsed fields.
pub fn decoder_configuration_record(
    vps: &[u8],
    sps: &[u8],
    pps: &[u8],
) -> Result<Vec<u8>, &'static str> {
    let sps_rbsp = rbsp(sps);
    let ptl = sps_rbsp
        .get(1..13)
        .ok_or("SPS too short to contain profile_tier_level")?;

    let mut record = Vec::with_capacity(64 + vps.len() + sps.len() + pps.len());
    record.push(1); // configurationVersion
    record.push(ptl[0]); // profile_space(2) | tier_flag(1) | profile_idc(5)
    record.extend_from_slice(&ptl[1..5]); // general_profile_compatibility_flags
    record.extend_from_slice(&ptl[5..11]); // general_constraint_indicator_flags
    record.push(ptl[11]); // general_level_idc
    record.extend_from_slice(&0xF000u16.to_be_bytes()); // reserved | min_spatial_segmentation_idc=0
    record.push(0xFC); // reserved | parallelismType=0
    record.push(0xFC | 0x01); // reserved | chroma_format_idc=1 (4:2:0)
    record.push(0xF8); // reserved | bit_depth_luma_minus8=0
    record.push(0xF8); // reserved | bit_depth_chroma_minus8=0
    record.extend_from_slice(&0u16.to_be_bytes()); // avgFrameRate=0 (unspecified)
                                                   // constantFrameRate(2)=0 | numTemporalLayers(3)=1 | temporalIdNested(1)=0
                                                   // | lengthSizeMinusOne(2)=3
    record.push((1 << 3) | 0x03);

    record.push(3); // numOfArrays: VPS, SPS, PPS
    for (nal_type, nal) in [(NAL_VPS, vps), (NAL_SPS, sps), (NAL_PPS, pps)] {
        // array_completeness(1)=0 | reserved(1)=0 | NAL_unit_type(6).
        // `hev1` allows in-band parameter sets, so completeness stays 0.
        record.push(nal_type);
        record.extend_from_slice(&1u16.to_be_bytes()); // numNalus
        record.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        record.extend_from_slice(nal);
    }

    Ok(record)
}

/// Concatenate NAL units into the hvcC sample format.
pub fn pack_access_unit(nals: &[Vec<u8>]) -> Vec<u8> {
    let total = nals.iter().map(|nal| nal.len() + 4).sum();
    let mut data = Vec::with_capacity(total);
    for nal in nals {
        data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        data.extend_from_slice(nal);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![nal_type << 1, 0x01];
        nal.extend_from_slice(body);
        nal
    }

    #[test]
    fn single_nal_passes_through() {
        let mut depacketizer = Depacketizer::new();
        let mut out = Vec::new();
        let input = nal(NAL_SPS, b"body");
        depacketizer.push(&input, &mut out);
        assert_eq!(out, vec![input]);
    }

    #[test]
    fn aggregation_packet_splits_into_nals() {
        let first = nal(NAL_VPS, b"vps");
        let second = nal(NAL_PPS, b"pps");

        let mut payload = vec![NAL_AP << 1, 0x01];
        for part in [&first, &second] {
            payload.extend_from_slice(&(part.len() as u16).to_be_bytes());
            payload.extend_from_slice(part);
        }

        let mut out = Vec::new();
        Depacketizer::new().push(&payload, &mut out);
        assert_eq!(out, vec![first, second]);
    }

    #[test]
    fn fragmentation_unit_reassembles_original_header() {
        let mut depacketizer = Depacketizer::new();
        let mut out = Vec::new();

        // Start fragment: FU header carries the real type (IDR_W_RADL).
        depacketizer.push(&[NAL_FU << 1, 0x01, 0x80 | NAL_IDR_W_RADL, b'a'], &mut out);
        assert!(out.is_empty(), "no NAL until the end fragment");

        depacketizer.push(&[NAL_FU << 1, 0x01, 0x40 | NAL_IDR_W_RADL, b'b'], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(nal_type(&out[0]), NAL_IDR_W_RADL);
        assert_eq!(&out[0][2..], b"ab");
    }

    #[test]
    fn displayservice_trailer_is_stripped() {
        let mut body = b"slice".to_vec();
        body.extend_from_slice(DISPLAYSERVICE_NAL_TRAILER);
        let input = nal(NAL_IDR_W_RADL, &body);

        let mut out = Vec::new();
        Depacketizer::new().push(&input, &mut out);
        assert_eq!(&out[0][2..], b"slice");
    }

    #[test]
    fn sequence_gap_drops_the_partial_fragment() {
        let mut depacketizer = Depacketizer::new();
        let mut out = Vec::new();

        depacketizer.push(&[NAL_FU << 1, 0x01, 0x80 | NAL_CRA, b'a'], &mut out);
        depacketizer.reset_fragment();
        // The end fragment now has no start fragment to attach to. Emitting it
        // would produce a NAL with no header, so it is dropped outright and the
        // access unit is failed at the marker instead.
        depacketizer.push(&[NAL_FU << 1, 0x01, 0x40 | NAL_CRA, b'b'], &mut out);
        assert!(out.is_empty(), "never stitches across a gap");
    }

    #[test]
    fn packs_length_prefixed_nalus() {
        let nals = vec![vec![0xAA, 0xBB], vec![0xCC]];
        assert_eq!(
            pack_access_unit(&nals),
            vec![0, 0, 0, 2, 0xAA, 0xBB, 0, 0, 0, 1, 0xCC]
        );
    }

    /// Main profile, level 4.0, from a real iPhone display stream.
    #[test]
    fn parses_codec_string_and_hvcc_from_a_real_sps() {
        // sps_video_parameter_set_id=0, max_sub_layers_minus1=0, nesting=1,
        // profile_space=0, tier=0, profile_idc=1, pcf=0x60000000,
        // constraints=0x900000000000, level_idc=120.
        let sps = {
            let mut sps = vec![NAL_SPS << 1, 0x01];
            sps.extend_from_slice(&[
                0x01, // vps_id(4)=0 | max_sub_layers(3)=0 | nesting(1)=1
                0x01, // profile_space(2)=0 | tier(1)=0 | profile_idc(5)=1
                0x60, 0x00, 0x00, 0x00, // general_profile_compatibility_flags
                0x90, 0x00, 0x00, 0x00, 0x00, 0x00, // constraint indicators
                0x78, // level_idc = 120
            ]);
            sps
        };

        assert_eq!(codec_string_from_sps(&sps).unwrap(), "hev1.1.6.L120.90");

        let record = decoder_configuration_record(&[NAL_VPS << 1, 1], &sps, &[NAL_PPS << 1, 1])
            .expect("hvcC");
        assert_eq!(record[0], 1, "configurationVersion");
        assert_eq!(record[1], 0x01, "profile_space | tier | profile_idc");
        assert_eq!(record[12], 0x78, "general_level_idc");
        assert_eq!(record[22], 3, "numOfArrays");
    }
}

#[cfg(test)]
mod dimension_tests {
    use super::*;

    /// The same real iPhone SPS the codec-string test uses, extended with the
    /// fields that follow profile_tier_level. 1179×2556 is an iPhone 13 screen.
    #[test]
    fn reads_frame_size_from_the_sps() {
        let mut sps = vec![NAL_SPS << 1, 0x01];
        sps.extend_from_slice(&[
            0x01, // vps_id(4)=0 | max_sub_layers_minus1(3)=0 | nesting(1)=1
            0x01, // profile_space(2)=0 | tier(1)=0 | profile_idc(5)=1
            0x60, 0x00, 0x00, 0x00, // general_profile_compatibility_flags
            0x90, 0x00, 0x00, 0x00, 0x00, 0x00, // constraint indicators
            0x96, // level_idc = 150
        ]);
        // sps_seq_parameter_set_id=0 (ue: 1), chroma_format_idc=1 (ue: 010),
        // pic_width=1179 (ue), pic_height=2556 (ue), bit-packed.
        sps.extend_from_slice(&golomb_bytes(&[0, 1, 1179, 2556]));

        assert_eq!(dimensions_from_sps(&sps), Some((1179, 2556)));
    }

    #[test]
    fn a_truncated_sps_reports_nothing_rather_than_a_wrong_size() {
        let sps = vec![NAL_SPS << 1, 0x01, 0x01, 0x01];
        assert_eq!(dimensions_from_sps(&sps), None);
    }

    /// Pack exp-Golomb values into bytes, MSB first.
    fn golomb_bytes(values: &[u64]) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        for &value in values {
            let coded = value + 1;
            let width = 64 - coded.leading_zeros();
            bits.resize(bits.len() + width as usize - 1, 0);
            for shift in (0..width).rev() {
                bits.push(((coded >> shift) & 1) as u8);
            }
        }
        bits.chunks(8)
            .map(|chunk| chunk.iter().fold(0u8, |byte, bit| (byte << 1) | bit) << (8 - chunk.len()))
            .collect()
    }
}
