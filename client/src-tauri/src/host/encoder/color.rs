//! Windows 硬件 MFT 可能忽略媒体类型上的色彩属性。把实际码流的参数集统一为
//! 桌面采集约定：BT.709 原色 / 矩阵、sRGB 传递函数、有限范围。
//! 只改色彩描述，保留参数集其余位（尺寸、时序、HRD、参考帧等）和所有画面数据。

use std::ops::Range;

use airhand_proto::session::Codec;
use anyhow::{Result, anyhow, bail};

const PRIMARIES: u64 = 1;
const TRANSFER: u64 = 13;
const MATRIX: u64 = 1;

pub(super) fn signal_sdr_color(codec: Codec, data: &mut Vec<u8>) -> Result<()> {
    let fixed = match codec {
        Codec::H264 => annexb(data, false)?,
        Codec::H265 => annexb(data, true)?,
        Codec::AV1 => av1(data)?,
        Codec::Unknown(_) => bail!("can't signal color for an unknown codec"),
    };
    if let Some(fixed) = fixed {
        *data = fixed;
    }
    Ok(())
}

type Edits = Vec<(Range<usize>, Vec<u8>)>;

fn apply_edits(data: &[u8], edits: Edits) -> Option<Vec<u8>> {
    if edits.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() + 128);
    let mut cursor = 0;
    for (range, value) in edits {
        out.extend_from_slice(&data[cursor..range.start]);
        out.extend_from_slice(&value);
        cursor = range.end;
    }
    out.extend_from_slice(&data[cursor..]);
    Some(out)
}

fn start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    for i in from..data.len().saturating_sub(2) {
        if data[i..i + 2] != [0, 0] {
            continue;
        }
        if data[i + 2] == 1 {
            return Some((i, i + 3));
        }
        if data[i + 2] == 0 && data.get(i + 3) == Some(&1) {
            return Some((i, i + 4));
        }
    }
    None
}

fn annexb(data: &[u8], hevc: bool) -> Result<Option<Vec<u8>>> {
    let mut current = start_code(data, 0);
    let mut edits = Vec::new();
    while let Some((_, payload)) = current {
        current = start_code(data, payload);
        let end = current.map_or(data.len(), |(start, _)| start);
        let nal = &data[payload..end];
        let is_sps = nal.first().is_some_and(|h| {
            if hevc {
                (h >> 1) & 0x3f == 33
            } else {
                h & 0x1f == 7
            }
        });
        if is_sps {
            let header = if hevc { 2 } else { 1 };
            let rbsp = unescape(nal.get(header..).ok_or_else(|| anyhow!("truncated SPS"))?)
                .ok_or_else(|| anyhow!("invalid SPS emulation prevention"))?;
            let fixed = if hevc {
                hevc_sps(&rbsp)
            } else {
                h264_sps(&rbsp)
            }
            .ok_or_else(|| {
                anyhow!(
                    "invalid {} SPS color syntax",
                    if hevc { "HEVC" } else { "H.264" }
                )
            })?;
            let mut out = nal[..header].to_vec();
            escape(&fixed, &mut out);
            if out != nal {
                edits.push((payload..end, out));
            }
        }
    }
    Ok(apply_edits(data, edits))
}

fn unescape(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for (i, &byte) in data.iter().enumerate() {
        if zeros >= 2 && byte == 3 {
            if *data.get(i + 1)? > 3 {
                return None;
            }
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    Some(out)
}

fn escape(data: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0;
    for &byte in data {
        if zeros >= 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
}

struct Bits<'a> {
    data: &'a [u8],
    /// 不含 trailing_one_bit 和对齐用的零；改写后按新的位置重新对齐。
    len: usize,
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let last = data.iter().rposition(|&b| b != 0)?;
        let len = last * 8 + 7 - data[last].trailing_zeros() as usize;
        Some(Self { data, len, pos: 0 })
    }

    fn read(&mut self, n: usize) -> Option<u64> {
        if n > 64 || n > self.len.saturating_sub(self.pos) {
            return None;
        }
        let mut value = 0;
        for _ in 0..n {
            value = value << 1 | ((self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1) as u64;
            self.pos += 1;
        }
        Some(value)
    }

    fn bit(&mut self) -> Option<bool> {
        Some(self.read(1)? != 0)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        if n > self.len.saturating_sub(self.pos) {
            return None;
        }
        self.pos += n;
        Some(())
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        ((1u64 << zeros) - 1 + self.read(zeros)?).try_into().ok()
    }

    fn se(&mut self) -> Option<i64> {
        let value = self.ue()? as i64;
        Some(if value & 1 != 0 {
            (value + 1) / 2
        } else {
            -value / 2
        })
    }
}

#[derive(Default)]
struct Writer {
    data: Vec<u8>,
    len: usize,
}

impl Writer {
    fn write(&mut self, n: usize, value: u64) {
        for i in (0..n).rev() {
            if self.len % 8 == 0 {
                self.data.push(0);
            }
            if value >> i & 1 != 0 {
                let last = self.data.len() - 1;
                self.data[last] |= 1 << (7 - self.len % 8);
            }
            self.len += 1;
        }
    }

    fn copy(&mut self, data: &[u8], range: Range<usize>) {
        for i in range {
            self.write(1, ((data[i / 8] >> (7 - i % 8)) & 1) as u64);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.write(1, 1);
        while self.len % 8 != 0 {
            self.write(1, 0);
        }
        self.data
    }
}

fn h264_sps(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = Bits::new(data)?;
    let profile = r.read(8)?;
    r.skip(16)?; // constraints, level_idc
    r.ue()?; // seq_parameter_set_id
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        let chroma = r.ue()?;
        if chroma > 3 {
            return None;
        }
        if chroma == 3 {
            r.skip(1)?; // separate_colour_plane_flag
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if r.bit()? {
                    let (mut last, mut next) = (8i64, 8i64);
                    for _ in 0..if i < 6 { 16 } else { 64 } {
                        if next != 0 {
                            next = (last + r.se()?).rem_euclid(256);
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        } // log2_max_pic_order_cnt_lsb_minus4
        1 => {
            r.skip(1)?;
            r.se()?;
            r.se()?;
            let count = r.ue()?;
            if count > 255 {
                return None;
            }
            for _ in 0..count {
                r.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    r.ue()?; // max_num_ref_frames
    r.skip(1)?;
    r.ue()?; // pic_width_in_mbs_minus1
    r.ue()?; // pic_height_in_map_units_minus1
    if !r.bit()? {
        r.skip(1)?;
    } // mb_adaptive_frame_field_flag
    r.skip(1)?; // direct_8x8_inference_flag
    if r.bit()? {
        for _ in 0..4 {
            r.ue()?;
        }
    }
    vui(r, false)
}

fn hevc_sps(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = Bits::new(data)?;
    r.skip(4)?;
    let layers = r.read(3)? as usize;
    if layers > 6 {
        return None;
    }
    r.skip(1)?;
    r.skip(96)?; // general profile_tier_level and level_idc
    let mut flags = [(false, false); 7];
    for flag in flags.iter_mut().take(layers) {
        *flag = (r.bit()?, r.bit()?);
    }
    if layers > 0 {
        r.skip((8 - layers) * 2)?;
    }
    for (profile, level) in flags.into_iter().take(layers) {
        if profile {
            r.skip(88)?;
        }
        if level {
            r.skip(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma = r.ue()?;
    if chroma > 3 {
        return None;
    }
    if chroma == 3 {
        r.skip(1)?;
    }
    r.ue()?; // width
    r.ue()?; // height
    if r.bit()? {
        for _ in 0..4 {
            r.ue()?;
        }
    }
    r.ue()?; // bit_depth_luma_minus8
    r.ue()?; // bit_depth_chroma_minus8
    let poc_bits = r.ue()?.checked_add(4)? as usize;
    if poc_bits > 16 {
        return None;
    }
    let all_layers = r.bit()?;
    for _ in if all_layers { 0 } else { layers }..=layers {
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    for _ in 0..6 {
        r.ue()?;
    } // coding / transform block sizes and hierarchy depths
    if r.bit()? && r.bit()? {
        for size in 0..4 {
            for _ in (0..6).step_by(if size == 3 { 3 } else { 1 }) {
                if !r.bit()? {
                    r.ue()?;
                } else {
                    if size > 1 {
                        r.se()?;
                    }
                    for _ in 0..(1usize << (4 + size * 2)).min(64) {
                        r.se()?;
                    }
                }
            }
        }
    }
    r.skip(2)?; // amp_enabled_flag, sample_adaptive_offset_enabled_flag
    if r.bit()? {
        r.skip(8)?;
        r.ue()?;
        r.ue()?;
        r.skip(1)?;
    }
    let count = r.ue()? as usize;
    if count > 64 {
        return None;
    }
    let mut delta_pocs = Vec::with_capacity(count);
    for i in 0..count {
        let mut pocs = 0;
        if i != 0 && r.bit()? {
            r.skip(1)?; // delta_rps_sign
            r.ue()?; // abs_delta_rps_minus1
            for _ in 0..=delta_pocs[i - 1] {
                let used = r.bit()?;
                if used || r.bit()? {
                    pocs += 1;
                }
            }
        } else {
            pocs = r.ue()?.checked_add(r.ue()?)? as usize;
            if pocs > 64 {
                return None;
            }
            for _ in 0..pocs {
                r.ue()?;
                r.skip(1)?;
            }
        }
        delta_pocs.push(pocs);
    }
    if r.bit()? {
        let count = r.ue()?;
        if count > 32 {
            return None;
        }
        for _ in 0..count {
            r.skip(poc_bits + 1)?;
        }
    }
    r.skip(2)?; // sps_temporal_mvp_enabled_flag, strong_intra_smoothing_enabled_flag
    vui(r, true)
}

/// H.264 和 HEVC 的 VUI 前三项语法相同。后面的时序和 HRD 原样保留。
fn vui(mut r: Bits<'_>, hevc: bool) -> Option<Vec<u8>> {
    let flag_pos = r.pos;
    let present = r.bit()?;
    let mut out = Writer::default();
    if present {
        if r.bit()? && r.read(8)? == 255 {
            r.skip(32)?;
        } // aspect ratio
        if r.bit()? {
            r.skip(1)?;
        } // overscan
        out.copy(r.data, 0..r.pos);
    } else {
        out.copy(r.data, 0..flag_pos);
        out.write(1, 1); // vui_parameters_present_flag
        out.write(2, 0); // aspect_ratio_info_present_flag, overscan_info_present_flag
    }
    let mut format = 5;
    if present && r.bit()? {
        format = r.read(3)?;
        r.skip(1)?; // old full_range_flag
        if r.bit()? {
            r.skip(24)?;
        }
    }
    out.write(1, 1); // video_signal_type_present_flag
    out.write(3, format);
    out.write(1, 0); // limited range
    out.write(1, 1); // colour_description_present_flag
    out.write(8, PRIMARIES);
    out.write(8, TRANSFER);
    out.write(8, MATRIX);
    if !present {
        // Minimal VUI, with no chroma location, display window, timing, HRD or restrictions.
        out.write(if hevc { 7 } else { 6 }, 0);
    }
    out.copy(r.data, r.pos..r.len);
    Some(out.finish())
}

fn leb128(data: &[u8], cursor: &mut usize) -> Option<usize> {
    let mut value = 0u64;
    for i in 0..8 {
        let byte = *data.get(*cursor)?;
        *cursor += 1;
        value |= ((byte & 0x7f) as u64) << (i * 7);
        if byte & 0x80 == 0 {
            return value.try_into().ok();
        }
    }
    None
}

fn write_leb128(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let rest = value >> 7;
        out.push((value & 0x7f) as u8 | if rest != 0 { 0x80 } else { 0 });
        value = rest;
        if value == 0 {
            break;
        }
    }
}

fn av1(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut cursor = 0;
    let mut edits = Vec::new();
    while cursor < data.len() {
        let start = cursor;
        let header = data[cursor];
        if header & 0x81 != 0 {
            bail!("invalid AV1 OBU header");
        }
        cursor += 1 + usize::from(header & 4 != 0);
        if cursor > data.len() {
            bail!("truncated AV1 OBU extension");
        }
        let header_end = cursor;
        let has_size = header & 2 != 0;
        let len = if has_size {
            leb128(data, &mut cursor).ok_or_else(|| anyhow!("invalid AV1 OBU size"))?
        } else {
            data.len() - cursor
        };
        let end = cursor
            .checked_add(len)
            .filter(|&n| n <= data.len())
            .ok_or_else(|| anyhow!("truncated AV1 OBU payload"))?;
        if (header >> 3) & 15 == 1 {
            let fixed = av1_sequence(&data[cursor..end])
                .ok_or_else(|| anyhow!("invalid AV1 sequence color syntax"))?;
            let mut out = data[start..header_end].to_vec();
            if has_size {
                write_leb128(fixed.len(), &mut out);
            }
            out.extend_from_slice(&fixed);
            if out != data[start..end] {
                edits.push((start..end, out));
            }
        }
        cursor = end;
    }
    Ok(apply_edits(data, edits))
}

fn av1_sequence(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = Bits::new(data)?;
    let profile = r.read(3)?;
    if profile > 2 {
        return None;
    }
    r.skip(1)?; // still_picture
    let reduced = r.bit()?;
    if reduced {
        r.skip(5)?;
    } else {
        let mut decoder_model = false;
        let mut delay_bits = 0;
        if r.bit()? {
            r.skip(64)?;
            if r.bit()? {
                r.ue()?;
            } // equal_picture_interval
            decoder_model = r.bit()?;
            if decoder_model {
                delay_bits = r.read(5)? as usize + 1;
                r.skip(42)?;
            }
        }
        let display_delay = r.bit()?;
        let count = r.read(5)? + 1;
        for _ in 0..count {
            r.skip(12)?;
            if r.read(5)? > 7 {
                r.skip(1)?;
            }
            if decoder_model && r.bit()? {
                r.skip(delay_bits * 2 + 1)?;
            }
            if display_delay && r.bit()? {
                r.skip(4)?;
            }
        }
    }
    let width_bits = r.read(4)? as usize + 1;
    let height_bits = r.read(4)? as usize + 1;
    r.skip(width_bits + height_bits)?;
    if !reduced && r.bit()? {
        r.skip(7)?;
    }
    r.skip(3)?;
    if !reduced {
        r.skip(4)?;
        let order_hint = r.bit()?;
        if order_hint {
            r.skip(2)?;
        }
        let screen = if r.bit()? { 2 } else { r.read(1)? };
        if screen > 0 && !r.bit()? {
            r.skip(1)?;
        }
        if order_hint {
            r.skip(3)?;
        }
    }
    r.skip(3)?; // superres, cdef, restoration
    let high = r.bit()?;
    if profile == 2 && high {
        r.skip(1)?;
    }
    let mono = profile != 1 && r.bit()?;
    let color_pos = r.pos;
    let (mut cp, mut tc, mut mc) = (2, 2, 2);
    if r.bit()? {
        cp = r.read(8)?;
        tc = r.read(8)?;
        mc = r.read(8)?;
    }
    // RGB identity coding omits color_range and has a different subsampling syntax.
    // It cannot be retagged as the NV12 input contract without changing coded pixels.
    if !mono && cp == 1 && tc == 13 && mc == 0 {
        return None;
    }
    r.skip(1)?; // old color_range
    let mut out = Writer::default();
    out.copy(r.data, 0..color_pos);
    out.write(1, 1); // color_description_present_flag
    out.write(8, PRIMARIES);
    out.write(8, TRANSFER);
    out.write(8, MATRIX);
    out.write(1, 0); // limited range
    out.copy(r.data, r.pos..r.len);
    Some(out.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Output, Stdio};

    fn format(codec: Codec) -> &'static str {
        match codec {
            Codec::H265 => "hevc",
            Codec::AV1 => "obu",
            _ => "h264",
        }
    }

    fn run(program: &str, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn probe(codec: Codec, data: &[u8]) -> serde_json::Value {
        let out = run(
            "ffprobe",
            &[
                "-v",
                "error",
                "-f",
                format(codec),
                "-i",
                "pipe:0",
                "-show_entries",
                "stream=codec_name,profile,level,width,height,sample_aspect_ratio,r_frame_rate,color_range,color_space,color_transfer,color_primaries",
                "-of",
                "json",
            ],
            data,
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["streams"][0].clone()
    }

    fn frame_hashes(codec: Codec, data: &[u8]) -> Vec<String> {
        let out = run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-threads",
                "1",
                "-f",
                format(codec),
                "-i",
                "pipe:0",
                "-f",
                "framemd5",
                "-",
            ],
            data,
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| line.rsplit(',').next().unwrap().trim().to_string())
            .collect()
    }

    #[test]
    fn metadata_updates_keep_coded_pixels_and_timing() {
        let Ok(encoders) = Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
        else {
            println!("ffmpeg isn't installed, not checking encoded color metadata");
            return;
        };
        if Command::new("ffprobe").arg("-version").output().is_err() {
            println!("ffprobe isn't installed, not checking encoded color metadata");
            return;
        }
        assert!(encoders.status.success());
        let encoders = String::from_utf8_lossy(&encoders.stdout);
        for (codec, encoder) in [
            (Codec::H264, "libx264"),
            (Codec::H265, "libx265"),
            (Codec::AV1, "libaom-av1"),
        ] {
            if !encoders
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(encoder))
            {
                println!("ffmpeg has no {encoder}, not checking {codec:?} samples");
                continue;
            }
            for explicit_color in [false, true] {
                let mut args = vec![
                    "-v",
                    "error",
                    "-filter_threads",
                    "1",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=2",
                    "-frames:v",
                    "2",
                    "-threads",
                    "1",
                ];
                args.extend(["-c:v", encoder]);
                match codec {
                    Codec::H264 => {
                        if explicit_color {
                            // High profile + a non-default scaling list exercises the SPS prefix.
                            args.extend([
                                "-preset",
                                "medium",
                                "-profile:v",
                                "high",
                                "-x264-params",
                                "cqm4=17,17,17,17,17,17,17,17,17,17,17,17,17,17,17,17",
                            ]);
                        } else {
                            args.extend(["-preset", "ultrafast"]);
                        }
                    }
                    Codec::H265 => args.extend([
                        "-preset",
                        "ultrafast",
                        "-x265-params",
                        "pools=1:frame-threads=1:log-level=error",
                    ]),
                    Codec::AV1 => args.extend(["-cpu-used", "8"]),
                    _ => unreachable!(),
                }
                if explicit_color {
                    if codec != Codec::H264 {
                        args.extend(["-pix_fmt", "yuv420p10le"]);
                    }
                    args.extend([
                        "-color_primaries",
                        "smpte170m",
                        "-color_trc",
                        "smpte170m",
                        "-colorspace",
                        "smpte170m",
                        "-color_range",
                        "pc",
                    ]);
                }
                args.extend(["-f", format(codec), "pipe:1"]);
                let original = run("ffmpeg", &args, &[]).stdout;
                let before = probe(codec, &original);
                let mut fixed = original.clone();
                signal_sdr_color(codec, &mut fixed).unwrap();
                let after = probe(codec, &fixed);
                assert_eq!(after["color_range"], "tv", "{codec:?}: {after}");
                assert_eq!(after["color_space"], "bt709", "{codec:?}: {after}");
                assert_eq!(
                    after["color_transfer"], "iec61966-2-1",
                    "{codec:?}: {after}"
                );
                assert_eq!(after["color_primaries"], "bt709", "{codec:?}: {after}");
                for field in [
                    "codec_name",
                    "profile",
                    "level",
                    "width",
                    "height",
                    "sample_aspect_ratio",
                    "r_frame_rate",
                ] {
                    assert_eq!(before[field], after[field], "{codec:?} {field}");
                }
                let hashes = frame_hashes(codec, &original);
                assert_eq!(hashes.len(), 2, "{codec:?}");
                assert_eq!(
                    hashes,
                    frame_hashes(codec, &fixed),
                    "{codec:?}: coded pixels changed"
                );
                let once = fixed.clone();
                signal_sdr_color(codec, &mut fixed).unwrap();
                assert_eq!(fixed, once, "{codec:?}: color update isn't idempotent");
                println!("{codec:?} explicit_color={explicit_color}: {after}");
            }
        }
    }

    #[test]
    fn truncated_parameter_sets_fail_without_changing_the_frame() {
        for (codec, mut data) in [
            (Codec::H264, vec![0, 0, 1, 0x67, 0]),
            (Codec::H265, vec![0, 0, 1, 0x42, 1, 0]),
            (Codec::AV1, vec![0x0a, 1, 0]),
        ] {
            let original = data.clone();
            assert!(signal_sdr_color(codec, &mut data).is_err());
            assert_eq!(data, original);
        }
    }
}
