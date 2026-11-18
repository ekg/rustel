//! In-memory WAV encoding of rendered PCM.
//!
//! The engine's own WAV writers target file paths; an Android app hands a
//! player bytes or a file it wrote itself, so the host encodes the container
//! here instead of spooling through the filesystem.

/// Encode interleaved `pcm` as a WAV file in memory: a 44-byte RIFF header
/// and 16-bit signed little-endian samples clamped to ±1.0, the same
/// encoding as the engine's PCM16 export (`rustel_audio::WavSampleFormat`,
/// whose writers only take paths).
///
/// `channels` is the interleave width, so `pcm.len()` must be a whole
/// number of frames. Returns the file bytes, or the reason the PCM cannot
/// be addressed by a RIFF chunk length.
pub fn pcm16_wav_bytes(sample_rate: u32, channels: u16, pcm: &[f32]) -> Result<Vec<u8>, String> {
    if channels == 0 {
        return Err("a WAV needs at least one channel".to_string());
    }
    if !pcm.len().is_multiple_of(usize::from(channels)) {
        return Err(format!(
            "the PCM holds {} samples, not a whole number of {channels}-channel frames",
            pcm.len()
        ));
    }
    let frames = pcm.len() / usize::from(channels);
    // Checked the same way the engine's own WAV writer checks the RIFF
    // bound: refuse instead of truncating a `u32` chunk length.
    let data_bytes = (frames as u64)
        .checked_mul(u64::from(channels))
        .and_then(|samples| samples.checked_mul(2))
        .ok_or_else(|| {
            format!("{frames} frames of {channels}-channel PCM overflowed the byte count")
        })?;
    if data_bytes
        .checked_add(36)
        .is_none_or(|with_header| with_header > u64::from(u32::MAX))
    {
        return Err(format!(
            "{frames} frames need {data_bytes} PCM bytes, which a RIFF file cannot address"
        ));
    }
    let block_align = channels * 2;
    let byte_rate = u64::from(sample_rate) * u64::from(block_align);
    if byte_rate > u64::from(u32::MAX) {
        return Err(format!("{sample_rate} Hz overflows the WAV byte rate"));
    }
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(byte_rate as u32).to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for sample in pcm {
        let quantised = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        wav.extend_from_slice(&quantised.to_le_bytes());
    }
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_field(wav: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(wav[at..at + 2].try_into().expect("a two-byte field"))
    }

    fn u32_field(wav: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(wav[at..at + 4].try_into().expect("a four-byte field"))
    }

    #[test]
    fn the_header_matches_the_pcm_length() {
        let pcm = [0.0_f32; 8];
        let wav = pcm16_wav_bytes(48_000, 2, &pcm).expect("eight samples encode");
        assert_eq!(wav.len(), 44 + 16);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_field(&wav, 4), 36 + 16);
        assert_eq!(u32_field(&wav, 40), 16);
        assert_eq!(u16_field(&wav, 20), 1);
        assert_eq!(u16_field(&wav, 22), 2);
        assert_eq!(u32_field(&wav, 24), 48_000);
        assert_eq!(u32_field(&wav, 28), 48_000 * 4);
        assert_eq!(u16_field(&wav, 32), 4);
        assert_eq!(u16_field(&wav, 34), 16);
    }

    #[test]
    fn pcm16_clamps_to_full_scale_and_quantises() {
        let wav = pcm16_wav_bytes(8_000, 1, &[1.5, 1.0, 0.5, 0.0, -1.0, -2.0])
            .expect("six samples encode");
        let samples: Vec<i16> = wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(
            samples,
            vec![i16::MAX, i16::MAX, 16_384, 0, -i16::MAX, -i16::MAX]
        );
    }

    #[test]
    fn pcm_without_whole_frames_or_with_no_channels_is_refused() {
        let cut = pcm16_wav_bytes(48_000, 2, &[0.5; 3]);
        assert!(cut.is_err(), "three samples are one and a half frames");
        let no_channels = pcm16_wav_bytes(48_000, 0, &[]);
        assert!(no_channels.is_err(), "zero channels is not a WAV");
    }
}
