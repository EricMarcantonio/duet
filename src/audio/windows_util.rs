//! Pure (OS-free) helpers for the Windows backend, compiled on Windows and in tests on
//! every host: sample-format decode/encode, channel mapping, linear resampling for the
//! software mirror, endpoint-id <-> `DeviceId` registry, and Bluetooth address extraction
//! from PnP instance paths.

use super::DeviceId;
use std::collections::VecDeque;

/// Reserved `DeviceId` of the synthetic "Duet mirror" output device.
pub const MIRROR_DEVICE_ID: DeviceId = u32::MAX - 1;
/// UID of the synthetic mirror device.
pub const MIRROR_UID: &str = "duet:mirror";

// ---------------------------------------------------------------------------
// Endpoint UIDs and ids
// ---------------------------------------------------------------------------

/// Duet's Windows UID: the MMDevice endpoint id, plus `|AA-BB-CC-DD-EE-FF` when the
/// endpoint belongs to a Bluetooth device whose address is known (so that
/// [`super::address_from_uid`] works on Windows too).
pub fn make_endpoint_uid(endpoint_id: &str, bt_address: Option<&str>) -> String {
    match bt_address {
        Some(a) => format!("{endpoint_id}|{a}"),
        None => endpoint_id.to_string(),
    }
}

/// Strip Duet's `|MAC` suffix to get back the MMDevice endpoint id.
pub fn endpoint_id_from_uid(uid: &str) -> &str {
    uid.split_once('|').map_or(uid, |(id, _)| id)
}

/// Maps endpoint id strings to small stable `DeviceId`s (1, 2, 3, ... in first-seen order).
/// An id never changes or gets reused for the lifetime of the registry (i.e. the process),
/// even if the endpoint disappears and comes back.
#[derive(Debug, Default)]
pub struct IdRegistry {
    ids: Vec<String>,
}

impl IdRegistry {
    pub fn id_for(&mut self, endpoint_id: &str) -> DeviceId {
        // MMDevice ids are GUID strings: compare case-insensitively, keep the original.
        if let Some(i) = self
            .ids
            .iter()
            .position(|e| e.eq_ignore_ascii_case(endpoint_id))
        {
            return i as DeviceId + 1;
        }
        self.ids.push(endpoint_id.to_string());
        self.ids.len() as DeviceId
    }

    /// The endpoint id for `id` (as first seen).
    pub fn endpoint_of(&self, id: DeviceId) -> Option<&str> {
        let i = (id as usize).checked_sub(1)?;
        self.ids.get(i).map(String::as_str)
    }
}

/// WinRT `BluetoothDevice::BluetoothAddress()` (48-bit, MSB first) -> "AA-BB-CC-DD-EE-FF".
pub fn format_bt_address(addr: u64) -> String {
    (0..6)
        .rev()
        .map(|i| format!("{:02X}", (addr >> (i * 8)) & 0xff))
        .collect::<Vec<_>>()
        .join("-")
}

/// True for the PnP enumerators used by Bluetooth audio (Classic A2DP, Hands-Free, LE).
pub fn is_bluetooth_enumerator(s: &str) -> bool {
    let u = s.to_ascii_uppercase();
    u.contains("BTHENUM")
        || u.contains("BTHHFENUM")
        || u.contains("BTHLEDEVICE")
        || u.contains("BTHLE\\")
}

/// Extract a Bluetooth address from a PnP instance id / device path such as
/// `{1}.BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&200e\7&2b3f5d5e&0&022A3B4C5D6E_C00000000`
/// or `BTHHFENUM\BthHFPAudio\8&1a2b3c4d&0&97` (no address -> None).
pub fn bt_address_from_instance_id(s: &str) -> Option<String> {
    if !is_bluetooth_enumerator(s) {
        return None;
    }
    // Only the instance part (after the last '\') holds the address; earlier parts contain
    // GUIDs whose last group is also 12 hex digits.
    let tail = s.rsplit('\\').next()?;
    let tok = tail
        .split(['&', '_', '#', '{', '}', '.'])
        .map(|t| {
            t.strip_prefix("Dev")
                .or_else(|| t.strip_prefix("dev"))
                .unwrap_or(t)
        })
        .find(|t| {
            t.len() == 12 && t.chars().all(|c| c.is_ascii_hexdigit()) && t.chars().any(|c| c != '0')
        })?;
    let up = tok.to_ascii_uppercase();
    Some(
        (0..6)
            .map(|i| &up[i * 2..i * 2 + 2])
            .collect::<Vec<_>>()
            .join("-"),
    )
}

// ---------------------------------------------------------------------------
// Sample formats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
    /// Packed 24-bit little-endian.
    I24,
    /// 32-bit container (also 24-in-32, which WASAPI left-justifies).
    I32,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::F32 | SampleFormat::I32 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub rate: u32,
    pub channels: u16,
    pub sample: SampleFormat,
}

impl StreamFormat {
    pub fn block_align(&self) -> usize {
        self.sample.bytes() * self.channels as usize
    }
}

pub const WAVE_FORMAT_PCM: u16 = 1;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Interpret a WAVEFORMATEX(-TENSIBLE). For EXTENSIBLE, pass the SubFormat's first data
/// field (`KSDATAFORMAT_SUBTYPE_PCM` = 1, `_IEEE_FLOAT` = 3) as `ext_subformat`.
pub fn stream_format(
    tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    ext_subformat: Option<u32>,
) -> Option<StreamFormat> {
    let tag = match (tag, ext_subformat) {
        (WAVE_FORMAT_EXTENSIBLE, Some(sub)) => u16::try_from(sub).ok()?,
        (WAVE_FORMAT_EXTENSIBLE, None) => return None,
        (t, _) => t,
    };
    let sample = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleFormat::F32,
        (WAVE_FORMAT_PCM, 16) => SampleFormat::I16,
        (WAVE_FORMAT_PCM, 24) => SampleFormat::I24,
        (WAVE_FORMAT_PCM, 32) => SampleFormat::I32,
        _ => return None,
    };
    (channels > 0 && rate > 0).then_some(StreamFormat {
        rate,
        channels,
        sample,
    })
}

/// Decode interleaved raw samples to f32 (appended to `out`).
pub fn decode_to_f32(bytes: &[u8], fmt: SampleFormat, out: &mut Vec<f32>) {
    match fmt {
        SampleFormat::F32 => out.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        ),
        SampleFormat::I16 => out.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0),
        ),
        SampleFormat::I24 => out.extend(
            bytes
                .as_chunks::<3>()
                .0
                .iter()
                .map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0),
        ),
        SampleFormat::I32 => out.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0),
        ),
    }
}

/// Encode f32 samples into `out` (must be `samples.len() * fmt.bytes()` long). Clamps.
pub fn encode_from_f32(samples: &[f32], fmt: SampleFormat, out: &mut [u8]) {
    let n = fmt.bytes();
    debug_assert_eq!(out.len(), samples.len() * n);
    for (s, o) in samples.iter().zip(out.chunks_exact_mut(n)) {
        let s = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match fmt {
            SampleFormat::F32 => o.copy_from_slice(&s.to_le_bytes()),
            SampleFormat::I16 => o.copy_from_slice(&((s * 32767.0).round() as i16).to_le_bytes()),
            SampleFormat::I24 => {
                let v = (s * 8_388_607.0).round() as i32;
                o.copy_from_slice(&v.to_le_bytes()[..3]);
            }
            SampleFormat::I32 => {
                o.copy_from_slice(&((s as f64 * 2_147_483_647.0).round() as i32).to_le_bytes())
            }
        }
    }
}

/// Re-map interleaved audio from `in_ch` to `out_ch` channels (appended to `out`).
/// Mono is duplicated to every output channel; downmix to mono averages; otherwise
/// channels are copied by position and extra output channels are silent.
pub fn map_channels(input: &[f32], in_ch: usize, out_ch: usize, out: &mut Vec<f32>) {
    if in_ch == 0 || out_ch == 0 {
        return;
    }
    if in_ch == out_ch {
        out.extend_from_slice(input);
        return;
    }
    for frame in input.chunks_exact(in_ch) {
        if in_ch == 1 {
            out.extend(std::iter::repeat_n(frame[0], out_ch));
        } else if out_ch == 1 {
            out.push(frame.iter().sum::<f32>() / in_ch as f32);
        } else {
            out.extend((0..out_ch).map(|c| frame.get(c).copied().unwrap_or(0.0)));
        }
    }
}

/// Streaming linear-interpolation resampler for interleaved f32.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    channels: usize,
    /// Input frames advanced per output frame (in_rate / out_rate).
    step: f64,
    /// Position of the next output frame, in input frames, relative to `last`
    /// (0.0 = exactly at `last`, 1.0 = at the first frame of the next chunk).
    pos: f64,
    /// Last input frame of the previous chunk (None before the first chunk).
    last: Option<Vec<f32>>,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            last: None,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.step == 1.0
    }

    /// Resample one chunk; output appended to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        let n = input.len() / ch;
        if n == 0 {
            return;
        }
        if self.is_passthrough() {
            out.extend_from_slice(&input[..n * ch]);
            return;
        }
        // Virtual sequence x[0] = last, x[1..=n] = input frames.
        let last = self.last.take().unwrap_or_else(|| input[..ch].to_vec());
        let frame = |i: usize| -> &[f32] {
            if i == 0 {
                &last
            } else {
                &input[(i - 1) * ch..i * ch]
            }
        };
        while self.pos < n as f64 {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            let (a, b) = (frame(i), frame(i + 1));
            out.extend((0..ch).map(|c| a[c] + (b[c] - a[c]) * t));
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.last = Some(input[(n - 1) * ch..n * ch].to_vec());
    }
}

/// Converts captured source audio (interleaved f32 at the source rate/channels) into a
/// target's channel count and rate, buffering the result.
#[derive(Debug)]
pub struct Converter {
    in_ch: usize,
    out_ch: usize,
    resampler: LinearResampler,
    scratch: Vec<f32>,
    /// Converted samples waiting to be rendered (interleaved, target channels).
    pub queue: VecDeque<f32>,
}

impl Converter {
    pub fn new(src: (u32, u16), dst: (u32, u16)) -> Self {
        Self {
            in_ch: src.1 as usize,
            out_ch: dst.1 as usize,
            resampler: LinearResampler::new(src.0, dst.0, dst.1 as usize),
            scratch: Vec::new(),
            queue: VecDeque::new(),
        }
    }

    pub fn push(&mut self, src: &[f32]) {
        self.scratch.clear();
        map_channels(src, self.in_ch, self.out_ch, &mut self.scratch);
        let mut res = Vec::with_capacity(self.scratch.len() + self.out_ch * 4);
        self.resampler.process(&self.scratch, &mut res);
        self.queue.extend(res);
    }

    /// Queued frames.
    pub fn frames(&self) -> usize {
        self.queue.len() / self.out_ch.max(1)
    }

    /// Drop the oldest frames so at most `max_frames` remain (bounds latency when the
    /// target's clock runs slower than the source's).
    pub fn trim_to(&mut self, max_frames: usize) {
        let excess = self.frames().saturating_sub(max_frames);
        self.queue.drain(..excess * self.out_ch);
    }

    /// Pop up to `max_frames` frames into `out` (cleared first). Returns frames popped.
    pub fn pop(&mut self, max_frames: usize, out: &mut Vec<f32>) -> usize {
        let frames = self.frames().min(max_frames);
        out.clear();
        out.extend(self.queue.drain(..frames * self.out_ch));
        frames
    }
}

/// Frames in `hns` 100-ns units at `rate`.
pub fn hns_to_frames(hns: i64, rate: u32) -> usize {
    (hns.max(0) as u128 * rate as u128 / 10_000_000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uids_and_registry() {
        let ep = "{0.0.0.00000000}.{1b9e2b6c-5b0a-4d3e-9a2c-1f3c9e0d7a11}";
        let uid = make_endpoint_uid(ep, Some("02-2A-3B-4C-5D-6E"));
        assert_eq!(endpoint_id_from_uid(&uid), ep);
        assert_eq!(
            super::super::address_from_uid(&uid).as_deref(),
            Some("02-2A-3B-4C-5D-6E")
        );
        assert_eq!(endpoint_id_from_uid(ep), ep);
        assert_eq!(make_endpoint_uid(ep, None), ep);

        let mut r = IdRegistry::default();
        assert_eq!(r.id_for("A"), 1);
        assert_eq!(r.id_for("b"), 2);
        assert_eq!(r.id_for("a"), 1);
        assert_eq!(r.endpoint_of(1), Some("A"));
        assert_eq!(r.endpoint_of(2), Some("b"));
        assert_eq!(r.endpoint_of(0), None);
        assert_eq!(r.endpoint_of(3), None);
        assert_eq!(r.endpoint_of(MIRROR_DEVICE_ID), None);
    }

    #[test]
    fn bt_instance_ids() {
        assert_eq!(
            bt_address_from_instance_id(
                r"{1}.BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&200e\7&2b3f5d5e&0&022A3B4C5D6E_C00000000"
            )
            .as_deref(),
            Some("02-2A-3B-4C-5D-6E")
        );
        assert_eq!(
            bt_address_from_instance_id(r"BTHHFENUM\{0000111e-0000-1000-8000-00805f9b34fb}_LOCALMFG&0002\8&3a3b1c2d&0&021B2C3D4E5F_C00000000")
                .as_deref(),
            Some("02-1B-2C-3D-4E-5F")
        );
        assert_eq!(
            bt_address_from_instance_id(
                r"BTHLEDevice\{0000184e-0000-1000-8000-00805f9b34fb}_Dev_VID&0201ab\7&1&0&021b2c3d4e5f"
            ),
            Some("02-1B-2C-3D-4E-5F".into())
        );
        assert_eq!(
            bt_address_from_instance_id(
                r"BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_LOCALMFG&0000\7&1&0&000000000000_00000000"
            ),
            None
        );
        assert_eq!(
            bt_address_from_instance_id(
                r"HDAUDIO\FUNC_01&VEN_10EC&DEV_0257&SUBSYS_17AA225D\4&1234&0&0001"
            ),
            None
        );
        assert_eq!(
            bt_address_from_instance_id(r"USB\VID_1235&PID_8210\022A3B4C5D6E"),
            None
        );
        assert_eq!(format_bt_address(0x022A_3B4C_5D6E), "02-2A-3B-4C-5D-6E");
        assert_eq!(format_bt_address(0x0000_0000_0001), "00-00-00-00-00-01");
        assert!(is_bluetooth_enumerator("BTHENUM"));
        assert!(is_bluetooth_enumerator("bthhfenum"));
        assert!(!is_bluetooth_enumerator("HDAUDIO"));
    }

    #[test]
    fn wave_formats() {
        let f = stream_format(WAVE_FORMAT_EXTENSIBLE, 2, 48000, 32, Some(3)).unwrap();
        assert_eq!(
            f,
            StreamFormat {
                rate: 48000,
                channels: 2,
                sample: SampleFormat::F32
            }
        );
        assert_eq!(f.block_align(), 8);
        assert_eq!(
            stream_format(WAVE_FORMAT_PCM, 2, 44100, 16, None)
                .unwrap()
                .sample,
            SampleFormat::I16
        );
        assert_eq!(
            stream_format(WAVE_FORMAT_EXTENSIBLE, 6, 48000, 24, Some(1))
                .unwrap()
                .block_align(),
            18
        );
        assert_eq!(
            stream_format(WAVE_FORMAT_EXTENSIBLE, 2, 48000, 32, None),
            None
        );
        assert_eq!(stream_format(WAVE_FORMAT_PCM, 2, 48000, 8, None), None);
        assert_eq!(stream_format(WAVE_FORMAT_PCM, 0, 48000, 16, None), None);
    }

    #[test]
    fn sample_roundtrip() {
        let src = [0.0f32, 0.5, -0.5, 1.0, -1.0, 0.25];
        for fmt in [
            SampleFormat::F32,
            SampleFormat::I16,
            SampleFormat::I24,
            SampleFormat::I32,
        ] {
            let mut bytes = vec![0u8; src.len() * fmt.bytes()];
            encode_from_f32(&src, fmt, &mut bytes);
            let mut back = Vec::new();
            decode_to_f32(&bytes, fmt, &mut back);
            assert_eq!(back.len(), src.len());
            for (a, b) in src.iter().zip(&back) {
                assert!((a - b).abs() < 1e-4, "{fmt:?}: {a} vs {b}");
            }
        }
        let mut bytes = [0u8; 2];
        encode_from_f32(&[7.0], SampleFormat::I16, &mut bytes);
        assert_eq!(i16::from_le_bytes(bytes), 32767);
        encode_from_f32(&[f32::NAN], SampleFormat::I16, &mut bytes);
        assert_eq!(i16::from_le_bytes(bytes), 0);
    }

    #[test]
    fn channel_mapping() {
        let mut o = Vec::new();
        map_channels(&[0.1, 0.2], 1, 2, &mut o);
        assert_eq!(o, [0.1, 0.1, 0.2, 0.2]);
        o.clear();
        map_channels(&[0.2, 0.4, 0.6, 0.8], 2, 1, &mut o);
        assert!((o[0] - 0.3).abs() < 1e-6 && (o[1] - 0.7).abs() < 1e-6);
        o.clear();
        map_channels(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, 2, &mut o);
        assert_eq!(o, [1.0, 2.0]);
        o.clear();
        map_channels(&[1.0, 2.0], 2, 4, &mut o);
        assert_eq!(o, [1.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn resampler_passthrough_and_ratio() {
        let mut r = LinearResampler::new(48000, 48000, 2);
        let mut o = Vec::new();
        r.process(&[1.0, 2.0, 3.0, 4.0], &mut o);
        assert_eq!(o, [1.0, 2.0, 3.0, 4.0]);

        // 44.1k -> 48k over many chunks: output length tracks the ratio, constant stays constant.
        let mut r = LinearResampler::new(44100, 48000, 1);
        let mut o = Vec::new();
        let chunk = vec![0.5f32; 441];
        for _ in 0..100 {
            r.process(&chunk, &mut o);
        }
        let expected = 44100.0 * 48000.0 / 44100.0;
        assert!((o.len() as f64 - expected).abs() <= 2.0, "{}", o.len());
        assert!(o.iter().all(|s| (s - 0.5).abs() < 1e-6));

        // Downsample 48k -> 16k (step 3) on a ramp: picks every third input sample, across chunk edges.
        let mut r = LinearResampler::new(48000, 16000, 1);
        let mut o = Vec::new();
        let ramp: Vec<f32> = (0..30).map(|i| i as f32).collect();
        r.process(&ramp[..10], &mut o);
        r.process(&ramp[10..], &mut o);
        assert_eq!(o, [0.0, 2.0, 5.0, 8.0, 11.0, 14.0, 17.0, 20.0, 23.0, 26.0]);
    }

    #[test]
    fn resampler_interpolates_ramp() {
        // 1 -> 2x upsample of a ramp yields half-steps, continuous across chunks.
        let mut r = LinearResampler::new(24000, 48000, 1);
        let mut o = Vec::new();
        r.process(&[0.0, 1.0, 2.0], &mut o);
        r.process(&[3.0, 4.0], &mut o);
        let d: Vec<f32> = o.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(d.iter().skip(2).all(|x| (x - 0.5).abs() < 1e-6), "{o:?}");
    }

    #[test]
    fn converter_queue() {
        let mut c = Converter::new((48000, 2), (48000, 2));
        c.push(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        assert_eq!(c.frames(), 3);
        c.trim_to(2);
        assert_eq!(c.frames(), 2);
        let mut out = Vec::new();
        assert_eq!(c.pop(1, &mut out), 1);
        assert_eq!(out, [0.3, 0.4]);
        assert_eq!(c.pop(10, &mut out), 1);
        assert_eq!(out, [0.5, 0.6]);
        assert_eq!(c.pop(10, &mut out), 0);

        let mut c = Converter::new((44100, 1), (48000, 2));
        c.push(&vec![0.25; 4410]);
        assert!((c.frames() as i64 - 4800).abs() <= 2);
        assert_eq!(c.queue.len() % 2, 0);
        assert_eq!(hns_to_frames(10_000_000, 48000), 48000);
        assert_eq!(hns_to_frames(200_000, 48000), 960);
        assert_eq!(hns_to_frames(-5, 48000), 0);
    }
}
