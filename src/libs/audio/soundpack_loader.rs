use crate::state::paths;
use crate::state::soundpack::{ SoundpackCache, SoundpackMetadata };
use std::collections::HashMap;
use std::sync::Arc;

use super::audio_context::AudioContext;
use super::engine::{ AudioCommand, EngineState };

/// Determine soundpack type based on the soundpack path
fn determine_soundpack_type(soundpack_id: &str) -> crate::state::soundpack::SoundpackType {
    if soundpack_id.starts_with("keyboard/") || soundpack_id.starts_with("keyboard\\") {
        crate::state::soundpack::SoundpackType::Keyboard
    } else if soundpack_id.starts_with("mouse/") || soundpack_id.starts_with("mouse\\") {
        crate::state::soundpack::SoundpackType::Mouse
    } else {
        // Default to keyboard for backwards compatibility
        crate::state::soundpack::SoundpackType::Keyboard
    }
}

/// Requests both keyboard and mouse soundpacks be (re)loaded from config.
/// Loading happens asynchronously on the engine thread - callers that need
/// to know the outcome should watch `UiEvent::PackLoaded` (see `ui.rs`)
/// rather than this function's return value, which only reflects whether
/// the request was sent.
pub fn load_soundpack(context: &AudioContext) -> Result<(), String> {
    let config = crate::state::config_writer::current();
    load_keyboard_soundpack(context, &config.keyboard_soundpack)?;
    load_mouse_soundpack(context, &config.mouse_soundpack)?;
    Ok(())
}

pub fn load_keyboard_soundpack(context: &AudioContext, soundpack_id: &str) -> Result<(), String> {
    load_keyboard_soundpack_with_cache_control(context, soundpack_id, true)
}

/// Sends a `LoadKeyboardPack` command to the engine thread. Errors from the
/// actual load (missing file, parse failure, etc.) surface later via
/// `UiEvent::PackLoaded`, not this function's return value.
pub fn load_keyboard_soundpack_with_cache_control(
    context: &AudioContext,
    soundpack_id: &str,
    update_cache_on_error: bool
) -> Result<(), String> {
    if soundpack_id.is_empty() {
        crate::always_print!("🎹 No keyboard soundpack selected");
        return Ok(());
    }

    crate::always_print!("🎹 Requesting keyboard soundpack load: {}", soundpack_id);
    context.send(AudioCommand::LoadKeyboardPack {
        soundpack_id: soundpack_id.to_string(),
        update_cache_on_error,
    });
    Ok(())
}

pub fn load_mouse_soundpack(context: &AudioContext, soundpack_id: &str) -> Result<(), String> {
    load_mouse_soundpack_with_cache_control(context, soundpack_id, true)
}

/// Sends a `LoadMousePack` command to the engine thread. See
/// `load_keyboard_soundpack_with_cache_control` for the async-result note.
pub fn load_mouse_soundpack_with_cache_control(
    context: &AudioContext,
    soundpack_id: &str,
    update_cache_on_error: bool
) -> Result<(), String> {
    if soundpack_id.is_empty() {
        crate::always_print!("🖱️ No mouse soundpack selected");
        return Ok(());
    }

    crate::always_print!("🖱️ Requesting mouse soundpack load: {}", soundpack_id);
    context.send(AudioCommand::LoadMousePack {
        soundpack_id: soundpack_id.to_string(),
        update_cache_on_error,
    });
    Ok(())
}

/// (samples, channels, sample_rate) for a decoded/resampled audio buffer.
type DecodedAudio = (Arc<Vec<f32>>, u16, u32);

/// Loads and decodes an audio file from path, resampling if needed.
fn load_audio_file_from_path(
    file_path: &str,
    device_rate: Option<u32>
) -> Result<(DecodedAudio, DecodedAudio), String> {
    if !std::path::Path::new(file_path).exists() {
        return Err(format!("Sound file not found: {}", file_path));
    }

    let (samples, channels, file_rate) = load_audio_with_symphonia(file_path).map_err(
        |e| format!("Failed to load audio '{}': {}", file_path, e)
    )?;

    match device_rate {
        Some(device_rate) if device_rate != file_rate => {
            let start = std::time::Instant::now();
            let resampled = super::resampler::resample_interleaved(
                &samples,
                channels,
                file_rate,
                device_rate
            );
            crate::always_print!(
                "🔁 Resampled soundpack audio {}Hz -> {}Hz in {:.1}ms",
                file_rate,
                device_rate,
                start.elapsed().as_secs_f64() * 1000.0
            );
            let original = (Arc::new(samples), channels, file_rate);
            let resampled_audio = (Arc::new(resampled), channels, device_rate);
            Ok((original, resampled_audio))
        }
        _ => {
            let samples_arc = Arc::new(samples);
            let decoded = (samples_arc.clone(), channels, file_rate);
            Ok((decoded.clone(), decoded))
        }
    }
}

fn find_audio_file_in_dir(dir: &str) -> Option<String> {
    let audio_extensions = ["ogg", "mp3", "wav", "flac"];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if let Some(filename) = entry.file_name().to_str() {
                let filename_lower = filename.to_lowercase();
                for ext in &audio_extensions {
                    if filename_lower.ends_with(&format!(".{}", ext)) {
                        return Some(entry.path().to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    None
}

/// Load audio file using Symphonia for consistent duration detection
fn load_audio_with_symphonia(file_path: &str) -> Result<(Vec<f32>, u16, u32), String> {
    use symphonia::core::audio::{ AudioBufferRef, Signal };
    use symphonia::core::codecs::{ DecoderOptions, CODEC_TYPE_NULL };
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    use std::fs::File;

    // First, check if file exists and has content
    let metadata = std::fs
        ::metadata(file_path)
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;
    if metadata.len() == 0 {
        return Err(format!("Audio file is empty: {}", file_path));
    }

    let file = File::open(file_path).map_err(|e| format!("Failed to open file: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = std::path::Path::new(file_path).extension() {
        if let Some(ext_str) = extension.to_str() {
            hint.with_extension(ext_str);
        }
    }

    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    let probed = symphonia::default
        ::get_probe()
        .format(&hint, mss, &fmt_opts, &meta_opts)
        .map_err(|e| {
            format!(
                "Failed to probe format for '{}': {} (file size: {} bytes)",
                file_path,
                e,
                metadata.len()
            )
        })?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No supported audio tracks found")?;

    let dec_opts: DecoderOptions = Default::default();
    let mut decoder = symphonia::default
        ::get_codecs()
        .make(&track.codec_params, &dec_opts)
        .map_err(|e| format!("Failed to create decoder: {}", e))?;

    let track_id = track.id;
    let mut samples = Vec::new();
    let mut sample_rate = 44100u32;
    let mut channels = 2u16;

    // Decode audio packets
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(_) => {
                break;
            } // End of stream
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                if samples.is_empty() {
                    // Get format info from first decoded buffer
                    sample_rate = decoded.spec().rate;
                    channels = decoded.spec().channels.count() as u16;
                } // Convert audio buffer to f32 samples
                match decoded {
                    AudioBufferRef::F32(buf) => {
                        if channels == 1 {
                            // Mono audio
                            for &sample in buf.chan(0) {
                                samples.push(sample);
                            }
                        } else {
                            // Stereo audio - interleave samples correctly
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push(*left);
                                samples.push(*right);
                            }
                        }
                    }
                    AudioBufferRef::S32(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push((sample as f32) / (i32::MAX as f32));
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push((*left as f32) / (i32::MAX as f32));
                                samples.push((*right as f32) / (i32::MAX as f32));
                            }
                        }
                    }
                    AudioBufferRef::S16(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push((sample as f32) / (i16::MAX as f32));
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push((*left as f32) / (i16::MAX as f32));
                                samples.push((*right as f32) / (i16::MAX as f32));
                            }
                        }
                    }
                    AudioBufferRef::U32(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push(
                                    ((sample as f32) - (u32::MAX as f32) / 2.0) /
                                        ((u32::MAX as f32) / 2.0)
                                );
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push(
                                    ((*left as f32) - (u32::MAX as f32) / 2.0) /
                                        ((u32::MAX as f32) / 2.0)
                                );
                                samples.push(
                                    ((*right as f32) - (u32::MAX as f32) / 2.0) /
                                        ((u32::MAX as f32) / 2.0)
                                );
                            }
                        }
                    }
                    AudioBufferRef::U16(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push(
                                    ((sample as f32) - (u16::MAX as f32) / 2.0) /
                                        ((u16::MAX as f32) / 2.0)
                                );
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push(
                                    ((*left as f32) - (u16::MAX as f32) / 2.0) /
                                        ((u16::MAX as f32) / 2.0)
                                );
                                samples.push(
                                    ((*right as f32) - (u16::MAX as f32) / 2.0) /
                                        ((u16::MAX as f32) / 2.0)
                                );
                            }
                        }
                    }
                    AudioBufferRef::U8(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push(((sample as f32) - 128.0) / 128.0);
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push(((*left as f32) - 128.0) / 128.0);
                                samples.push(((*right as f32) - 128.0) / 128.0);
                            }
                        }
                    }
                    AudioBufferRef::S8(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push((sample as f32) / (i8::MAX as f32));
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push((*left as f32) / (i8::MAX as f32));
                                samples.push((*right as f32) / (i8::MAX as f32));
                            }
                        }
                    }
                    AudioBufferRef::F64(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                samples.push(sample as f32);
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                samples.push(*left as f32);
                                samples.push(*right as f32);
                            }
                        }
                    }
                    AudioBufferRef::U24(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                let sample_f32 = ((sample.inner() as f32) - 8388608.0) / 8388608.0; // 2^23
                                samples.push(sample_f32);
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                let left_f32 = ((left.inner() as f32) - 8388608.0) / 8388608.0;
                                let right_f32 = ((right.inner() as f32) - 8388608.0) / 8388608.0;
                                samples.push(left_f32);
                                samples.push(right_f32);
                            }
                        }
                    }
                    AudioBufferRef::S24(buf) => {
                        if channels == 1 {
                            for &sample in buf.chan(0) {
                                let sample_f32 = (sample.inner() as f32) / 8388607.0; // 2^23 - 1
                                samples.push(sample_f32);
                            }
                        } else {
                            let left_chan = buf.chan(0);
                            let right_chan = if buf.spec().channels.count() > 1 {
                                buf.chan(1)
                            } else {
                                buf.chan(0)
                            };
                            for (left, right) in left_chan.iter().zip(right_chan.iter()) {
                                let left_f32 = (left.inner() as f32) / 8388607.0;
                                let right_f32 = (right.inner() as f32) / 8388607.0;
                                samples.push(left_f32);
                                samples.push(right_f32);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                crate::always_print!("⚠️ [DEBUG] Decode error (continuing): {}", e);
                continue;
            }
        }
    }

    if samples.is_empty() {
        return Err("No audio data decoded".to_string());
    }

    Ok((samples, channels, sample_rate))
}

/// Derives the `type/name` id the cache is keyed by from a pack's absolute
/// path, trying both soundpack roots.
///
/// Kept separate from the two roots themselves so the path logic can be tested
/// without a filesystem: `relative_soundpack_id` does the work.
fn soundpack_id_from_path(soundpack_path: &str) -> String {
    let roots = [
        crate::utils::path::get_soundpacks_dir_absolute(),
        crate::utils::path::get_custom_soundpacks_dir_absolute(),
    ];
    relative_soundpack_id(soundpack_path, &roots)
}

/// The id for `soundpack_path` relative to whichever of `roots` contains it.
///
/// Falls back to the last two path components rather than the folder name
/// alone: an id without its `keyboard/`/`mouse/` prefix does not match the key
/// the directory scanner uses, and inserting under it duplicates the pack in
/// the cache and in every list rendered from it.
fn relative_soundpack_id(soundpack_path: &str, roots: &[String]) -> String {
    let path = std::path::Path::new(soundpack_path);

    for root in roots {
        if let Ok(relative) = path.strip_prefix(root) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }

    let mut tail: Vec<&str> = path
        .components()
        .rev()
        .take(2)
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    tail.reverse();

    if tail.is_empty() { "unknown".to_string() } else { tail.join("/") }
}

fn create_soundpack_metadata(
    soundpack_path: &str,
    config: &serde_json::Value,
    soundpack_id: &str
) -> Result<SoundpackMetadata, String> {
    let id = soundpack_id_from_path(soundpack_path);

    let last_modified = match std::fs::metadata(soundpack_path) {
        Ok(metadata) =>
            metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        Err(_) => 0,
    };

    let name = config
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(soundpack_id)
        .to_string();

    let version = config
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("1.0")
        .to_string();

    let tags = config
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(SoundpackMetadata {
        id: id.clone(),
        name,
        author: config
            .get("author")
            .or_else(|| config.get("m_author"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        description: config
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        version,
        tags,
        icon: {
            if let Some(icon_filename) = config.get("icon").and_then(|v| v.as_str()) {
                let icon_path = format!("{}/{}", soundpack_path, icon_filename);
                if std::path::Path::new(&icon_path).exists() {
                    Some(format!("/soundpack-images/{}/{}", id, icon_filename))
                } else {
                    Some(String::new())
                }
            } else {
                Some(String::new())
            }
        },
        soundpack_type: determine_soundpack_type(soundpack_id),
        folder_path: id,
        last_modified,
        last_accessed: std::time::SystemTime
            ::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        config_version: Some(2),
        is_valid_v2: true,
        validation_status: "valid".to_string(),
        can_be_converted: false,
        last_error: None,
    })
}

/// Translates a mouse pack's definition key into the button code the runtime
/// actually emits.
fn mouse_button_for_definition(definition: &str) -> &str {
    match definition {
        "Escape" => "MouseLeft",
        "Digit1" => "MouseRight",
        "Digit2" => "MouseMiddle",
        other => other,
    }
}

/// Loads a keyboard soundpack directly into engine-owned state (Phase 3).
/// Mirrors `load_keyboard_soundpack_optimized` but writes to `EngineState`
/// fields instead of an `&AudioContext`, since the engine thread owns its
/// data as plain fields rather than `Arc<Mutex<...>>`.
pub(super) fn load_keyboard_pack_into_engine(
    state: &mut EngineState,
    soundpack_id: &str,
    update_cache_on_error: bool
) -> Result<String, String> {
    if soundpack_id.is_empty() {
        return Err("empty soundpack ID".to_string());
    }

    match load_keyboard_pack_into_engine_inner(state, soundpack_id) {
        Ok(name) => Ok(name),
        Err(e) => {
            if update_cache_on_error {
                capture_soundpack_loading_error(soundpack_id, &e);
            }
            Err(e)
        }
    }
}

fn load_keyboard_pack_into_engine_inner(
    state: &mut EngineState,
    soundpack_id: &str
) -> Result<String, String> {
    let soundpack_path = paths::soundpacks::soundpack_dir(soundpack_id);
    let config_path = paths::soundpacks::config_json(soundpack_id);
    let config_content = std::fs
        ::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config: {}", e))?;
    let config: serde_json::Value = serde_json
        ::from_str(&config_content)
        .map_err(|e| format!("Failed to parse soundpack config: {}", e))?;

    let soundpack_type = determine_soundpack_type(soundpack_id);
    if soundpack_type != crate::state::soundpack::SoundpackType::Keyboard {
        return Err("This is a mouse soundpack, not a keyboard soundpack".to_string());
    }

    state.keyboard_samples = None;
    state.keyboard_samples_original = None;
    state.key_map.clear();
    state.keyboard_multi_samples.clear();
    state.keyboard_multi_samples_original.clear();
    state.key_sinks.clear();

    let name = config
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(soundpack_id)
        .to_string();

    let mut file_cache: HashMap<String, (DecodedAudio, DecodedAudio)> = HashMap::new();
    let mut get_or_load_file = |rel_path: &str| -> Result<(DecodedAudio, DecodedAudio), String> {
        let full_path = format!("{}/{}", soundpack_path, rel_path.trim_start_matches("./"));
        if let Some(cached) = file_cache.get(&full_path) {
            return Ok(cached.clone());
        }
        let loaded = load_audio_file_from_path(&full_path, state.device_rate)?;
        file_cache.insert(full_path, loaded.clone());
        Ok(loaded)
    };

    let definitions = config.get("definitions").or_else(|| config.get("defs"));
    let defines = config.get("defines");
    let is_multi_method = config.get("definition_method").and_then(|v| v.as_str()) == Some("multi")
        || config.get("key_define_type").and_then(|v| v.as_str()) == Some("multi");

    if let Some(defs) = definitions.and_then(|d| d.as_object()) {
        let has_audio_file_in_defs = is_multi_method || defs.values().any(|v| {
            v.get("sound").is_some() || v.get("sounds").is_some() || v.get("audio_file").is_some()
        });

        if has_audio_file_in_defs {
            for (key_name, val) in defs {
                let mut filenames = Vec::new();
                if let Some(sound) = val.get("sound").and_then(|v| v.as_str()) {
                    filenames.push(sound.to_string());
                } else if let Some(audio_file) = val.get("audio_file").and_then(|v| v.as_str()) {
                    filenames.push(audio_file.to_string());
                } else if let Some(sounds) = val.get("sounds").and_then(|v| v.as_array()) {
                    for s in sounds {
                        if let Some(str_val) = s.as_str() {
                            filenames.push(str_val.to_string());
                        }
                    }
                }
                for filename in filenames {
                    if let Ok((orig, resampled)) = get_or_load_file(&filename) {
                        state.keyboard_multi_samples.entry(key_name.clone()).or_default().push(resampled);
                        state.keyboard_multi_samples_original.entry(key_name.clone()).or_default().push(orig);
                    }
                }
            }
        } else {
            let main_audio = config
                .get("audio_file")
                .or_else(|| config.get("sound"))
                .and_then(|v| v.as_str());
            let main_audio_path = if let Some(audio) = main_audio {
                format!("{}/{}", soundpack_path, audio.trim_start_matches("./"))
            } else {
                find_audio_file_in_dir(&soundpack_path).ok_or("No audio file found in soundpack")?
            };

            let (original, resampled) = load_audio_file_from_path(&main_audio_path, state.device_rate)?;
            state.keyboard_samples = Some(resampled);
            state.keyboard_samples_original = Some(original);

            for (key, val) in defs {
                let timings = match val.get("timing") {
                    Some(timing) => timing,
                    None => val,
                };
                if let Some(arr) = timings.as_array() {
                    for timing in arr {
                        if let Some(timing_arr) = timing.as_array() {
                            if timing_arr.len() >= 2 {
                                let start = timing_arr[0].as_f64().unwrap_or(0.0) as f32;
                                let end = timing_arr[1].as_f64().unwrap_or(100.0) as f32;
                                state.key_map.entry(key.clone()).or_default().push([start, end]);
                            }
                        }
                    }
                }
            }
        }
    } else if let Some(defs) = defines.and_then(|d| d.as_object()) {
        let iohook_mapping = crate::utils::config_converter::create_iohook_to_web_key_mapping();
        let is_multi_files = is_multi_method || defs.values().any(|v| {
            v.is_string()
                || (v.is_array()
                    && v.as_array().map_or(false, |a| {
                        a.first().map_or(false, |first| first.is_string())
                    }))
        });

        if is_multi_files {
            for (code_str, val) in defs {
                if let Some((iohook_num, _)) = crate::utils::config_converter::iohook_code_and_press(code_str) {
                    if let Some(key_name) = iohook_mapping.get(&iohook_num) {
                        let mut filenames = Vec::new();
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() && s != "null" {
                                filenames.push(s.to_string());
                            }
                        } else if let Some(arr) = val.as_array() {
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    if !s.is_empty() && s != "null" {
                                        filenames.push(s.to_string());
                                    }
                                }
                            }
                        }
                        for filename in filenames {
                            if let Ok((orig, resampled)) = get_or_load_file(&filename) {
                                state.keyboard_multi_samples.entry(key_name.clone()).or_default().push(resampled);
                                state.keyboard_multi_samples_original.entry(key_name.clone()).or_default().push(orig);
                            }
                        }
                    }
                }
            }
        } else {
            let main_audio = config
                .get("sound")
                .or_else(|| config.get("audio_file"))
                .and_then(|v| v.as_str());
            let main_audio_path = if let Some(audio) = main_audio {
                format!("{}/{}", soundpack_path, audio.trim_start_matches("./"))
            } else {
                find_audio_file_in_dir(&soundpack_path).ok_or("No audio file found in soundpack")?
            };

            let (original, resampled) = load_audio_file_from_path(&main_audio_path, state.device_rate)?;
            state.keyboard_samples = Some(resampled);
            state.keyboard_samples_original = Some(original);

            for (code_str, val) in defs {
                if let Some((iohook_num, _)) = crate::utils::config_converter::iohook_code_and_press(code_str) {
                    if let Some(key_name) = iohook_mapping.get(&iohook_num) {
                        if let Some(arr) = val.as_array() {
                            if arr.len() >= 2 {
                                let start = arr[0].as_f64().unwrap_or(0.0) as f32;
                                let dur = arr[1].as_f64().unwrap_or(100.0) as f32;
                                let end = start + dur;
                                state.key_map.entry(key_name.clone()).or_default().push([start, end]);
                            }
                        }
                    }
                }
            }
        }
    }

    update_soundpack_cache(&soundpack_path, &config, soundpack_id);
    crate::always_print!("✅ [Engine] Loaded keyboard soundpack: {}", name);
    Ok(name)
}

/// Loads a mouse soundpack directly into engine-owned state (Phase 3).
/// See `load_keyboard_pack_into_engine` for the rationale.
pub(super) fn load_mouse_pack_into_engine(
    state: &mut EngineState,
    soundpack_id: &str,
    update_cache_on_error: bool
) -> Result<String, String> {
    if soundpack_id.is_empty() {
        return Err("empty soundpack ID".to_string());
    }

    match load_mouse_pack_into_engine_inner(state, soundpack_id) {
        Ok(name) => Ok(name),
        Err(e) => {
            if update_cache_on_error {
                capture_soundpack_loading_error(soundpack_id, &e);
            }
            Err(e)
        }
    }
}

fn load_mouse_pack_into_engine_inner(
    state: &mut EngineState,
    soundpack_id: &str
) -> Result<String, String> {
    let soundpack_path = paths::soundpacks::soundpack_dir(soundpack_id);
    let config_path = paths::soundpacks::config_json(soundpack_id);
    let config_content = std::fs
        ::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config: {}", e))?;
    let config: serde_json::Value = serde_json
        ::from_str(&config_content)
        .map_err(|e| format!("Failed to parse config: {}", e))?;

    let soundpack_type = determine_soundpack_type(soundpack_id);
    if soundpack_type != crate::state::soundpack::SoundpackType::Mouse {
        return Err("This is a keyboard soundpack, not a mouse soundpack".to_string());
    }

    state.mouse_samples = None;
    state.mouse_samples_original = None;
    state.mouse_map.clear();
    state.mouse_multi_samples.clear();
    state.mouse_multi_samples_original.clear();
    state.mouse_sinks.clear();

    let name = config
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(soundpack_id)
        .to_string();

    let mut file_cache: HashMap<String, (DecodedAudio, DecodedAudio)> = HashMap::new();
    let mut get_or_load_file = |rel_path: &str| -> Result<(DecodedAudio, DecodedAudio), String> {
        let full_path = format!("{}/{}", soundpack_path, rel_path.trim_start_matches("./"));
        if let Some(cached) = file_cache.get(&full_path) {
            return Ok(cached.clone());
        }
        let loaded = load_audio_file_from_path(&full_path, state.device_rate)?;
        file_cache.insert(full_path, loaded.clone());
        Ok(loaded)
    };

    let definitions = config.get("definitions").or_else(|| config.get("defs"));
    let defines = config.get("defines");
    let is_multi_method = config.get("definition_method").and_then(|v| v.as_str()) == Some("multi")
        || config.get("key_define_type").and_then(|v| v.as_str()) == Some("multi");

    if let Some(defs) = definitions.and_then(|d| d.as_object()) {
        let has_audio_file_in_defs = is_multi_method || defs.values().any(|v| {
            v.get("sound").is_some() || v.get("sounds").is_some() || v.get("audio_file").is_some()
        });

        if has_audio_file_in_defs {
            for (key_name, val) in defs {
                let mapped_key = mouse_button_for_definition(key_name);
                let mut filenames = Vec::new();
                if let Some(sound) = val.get("sound").and_then(|v| v.as_str()) {
                    filenames.push(sound.to_string());
                } else if let Some(audio_file) = val.get("audio_file").and_then(|v| v.as_str()) {
                    filenames.push(audio_file.to_string());
                } else if let Some(sounds) = val.get("sounds").and_then(|v| v.as_array()) {
                    for s in sounds {
                        if let Some(str_val) = s.as_str() {
                            filenames.push(str_val.to_string());
                        }
                    }
                }
                for filename in filenames {
                    if let Ok((orig, resampled)) = get_or_load_file(&filename) {
                        state.mouse_multi_samples.entry(mapped_key.to_string()).or_default().push(resampled);
                        state.mouse_multi_samples_original.entry(mapped_key.to_string()).or_default().push(orig);
                    }
                }
            }
        } else {
            let main_audio = config
                .get("audio_file")
                .or_else(|| config.get("sound"))
                .and_then(|v| v.as_str());
            let main_audio_path = if let Some(audio) = main_audio {
                format!("{}/{}", soundpack_path, audio.trim_start_matches("./"))
            } else {
                find_audio_file_in_dir(&soundpack_path).ok_or("No audio file found in soundpack")?
            };

            let (original, resampled) = load_audio_file_from_path(&main_audio_path, state.device_rate)?;
            state.mouse_samples = Some(resampled);
            state.mouse_samples_original = Some(original);

            for (key, val) in defs {
                let mapped_key = mouse_button_for_definition(key);
                let timings = match val.get("timing") {
                    Some(timing) => timing,
                    None => val,
                };
                if let Some(arr) = timings.as_array() {
                    for timing in arr {
                        if let Some(timing_arr) = timing.as_array() {
                            if timing_arr.len() >= 2 {
                                let start = timing_arr[0].as_f64().unwrap_or(0.0) as f32;
                                let end = timing_arr[1].as_f64().unwrap_or(100.0) as f32;
                                state.mouse_map.entry(mapped_key.to_string()).or_default().push([start, end]);
                            }
                        }
                    }
                }
            }
        }
    } else if let Some(defs) = defines.and_then(|d| d.as_object()) {
        let iohook_mapping = crate::utils::config_converter::create_iohook_to_mouse_button_mapping();
        let is_multi_files = is_multi_method || defs.values().any(|v| {
            v.is_string()
                || (v.is_array()
                    && v.as_array().map_or(false, |a| {
                        a.first().map_or(false, |first| first.is_string())
                    }))
        });

        if is_multi_files {
            for (code_str, val) in defs {
                if let Some((iohook_num, _)) = crate::utils::config_converter::iohook_code_and_press(code_str) {
                    if let Some(key_name) = iohook_mapping.get(&iohook_num) {
                        let mut filenames = Vec::new();
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() && s != "null" {
                                filenames.push(s.to_string());
                            }
                        } else if let Some(arr) = val.as_array() {
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    if !s.is_empty() && s != "null" {
                                        filenames.push(s.to_string());
                                    }
                                }
                            }
                        }
                        for filename in filenames {
                            if let Ok((orig, resampled)) = get_or_load_file(&filename) {
                                state.mouse_multi_samples.entry(key_name.clone()).or_default().push(resampled);
                                state.mouse_multi_samples_original.entry(key_name.clone()).or_default().push(orig);
                            }
                        }
                    }
                }
            }
        } else {
            let main_audio = config
                .get("sound")
                .or_else(|| config.get("audio_file"))
                .and_then(|v| v.as_str());
            let main_audio_path = if let Some(audio) = main_audio {
                format!("{}/{}", soundpack_path, audio.trim_start_matches("./"))
            } else {
                find_audio_file_in_dir(&soundpack_path).ok_or("No audio file found in soundpack")?
            };

            let (original, resampled) = load_audio_file_from_path(&main_audio_path, state.device_rate)?;
            state.mouse_samples = Some(resampled);
            state.mouse_samples_original = Some(original);

            for (code_str, val) in defs {
                if let Some((iohook_num, _)) = crate::utils::config_converter::iohook_code_and_press(code_str) {
                    if let Some(key_name) = iohook_mapping.get(&iohook_num) {
                        if let Some(arr) = val.as_array() {
                            if arr.len() >= 2 {
                                let start = arr[0].as_f64().unwrap_or(0.0) as f32;
                                let dur = arr[1].as_f64().unwrap_or(100.0) as f32;
                                let end = start + dur;
                                state.mouse_map.entry(key_name.clone()).or_default().push([start, end]);
                            }
                        }
                    }
                }
            }
        }
    }

    update_soundpack_cache(&soundpack_path, &config, soundpack_id);
    crate::always_print!("✅ [Engine] Loaded mouse soundpack: {}", name);
    Ok(name)
}

/// Shared metadata-cache update used by both engine loaders.
fn update_soundpack_cache(soundpack_path: &str, config: &serde_json::Value, soundpack_id: &str) {
    let mut cache = SoundpackCache::load();
    match create_soundpack_metadata(soundpack_path, config, soundpack_id) {
        Ok(metadata) => {
            cache.add_soundpack(metadata);
        }
        Err(e) => {
            crate::always_print!("⚠️ Failed to create metadata for {}: {}", soundpack_id, e);
        }
    }
    cache.save();
}

/// Capture soundpack loading error and update the cache
fn capture_soundpack_loading_error(soundpack_id: &str, error: &str) {
    // Skip creating cache entries for empty soundpack IDs
    if soundpack_id.is_empty() {
        crate::always_print!("⚠️ Skipping cache entry for empty soundpack ID: {}", error);
        return;
    }

    crate::always_print!("📝 Capturing loading error for {}: {}", soundpack_id, error);

    let mut cache = SoundpackCache::load();

    // Check if we already have metadata for this soundpack
    if let Some(existing_metadata) = cache.soundpacks.get_mut(soundpack_id) {
        // Update existing metadata with error
        existing_metadata.last_error = Some(error.to_string());
        existing_metadata.validation_status = "loading_error".to_string();
    } else {
        // Create minimal metadata entry with error information
        let error_metadata = SoundpackMetadata {
            id: soundpack_id.to_string(),
            name: format!("Error: {}", soundpack_id),
            author: None,
            description: Some(format!("Loading failed: {}", error)),
            version: "unknown".to_string(),
            tags: vec!["error".to_string()],
            icon: None,
            soundpack_type: determine_soundpack_type(soundpack_id),
            folder_path: soundpack_id.to_string(), // Add folder_path for error entries
            last_modified: 0,
            last_accessed: std::time::SystemTime
                ::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_version: None,
            is_valid_v2: false,
            validation_status: "loading_error".to_string(),
            can_be_converted: false,
            last_error: Some(error.to_string()),
        };

        cache.soundpacks.insert(soundpack_id.to_string(), error_metadata);
    }

    cache.save();
    crate::always_print!("💾 Updated cache with error information for {}", soundpack_id);
}

#[cfg(test)]
mod tests {
    use super::{ mouse_button_for_definition, relative_soundpack_id };

    /// Joins with the running platform's separator. `Path::components` only
    /// splits on the native one, so a hard-coded `\` is a single component on
    /// Linux and the assertions below would be testing nothing there.
    fn native_path(parts: &[&str]) -> String {
        parts.join(std::path::MAIN_SEPARATOR_STR)
    }

    fn builtin_root() -> String {
        native_path(&["", "opt", "MechvibesDX", "soundpacks"])
    }

    fn custom_root() -> String {
        native_path(&["", "home", "someone", ".local", "share", "mechvibes", "soundpacks"])
    }

    fn roots() -> Vec<String> {
        vec![builtin_root(), custom_root()]
    }

    #[test]
    fn an_imported_pack_gets_the_same_id_the_scanner_uses() {
        // Only the built-in root used to be tried, so a pack in app data fell
        // through to a bare folder name. The scanner keys the cache by
        // `mouse/Viper Mini`, so inserting `Viper Mini` alongside it left two
        // entries for one pack and the selector listed it twice. Built-in
        // packs matched the first root and never showed the bug.
        assert_eq!(
            relative_soundpack_id(
                &native_path(&[&custom_root(), "mouse", "Viper Mini"]),
                &roots()
            ),
            "mouse/Viper Mini"
        );
        assert_eq!(
            relative_soundpack_id(
                &native_path(&[&builtin_root(), "keyboard", "eg-oreo"]),
                &roots()
            ),
            "keyboard/eg-oreo"
        );
    }

    #[test]
    fn a_path_under_no_known_root_still_keeps_its_type_prefix() {
        // The fallback keeps two components rather than one, so even an
        // unexpected location cannot produce a prefix-less id.
        assert_eq!(
            relative_soundpack_id(
                &native_path(&["", "elsewhere", "mouse", "Model O"]),
                &roots()
            ),
            "mouse/Model O"
        );
    }

    #[test]
    fn v1_era_definition_keys_map_onto_real_mouse_buttons() {
        // Packs converted by an earlier build carry the keyboard names the old
        // converter produced from iohook codes 1/2/3. Left silent, they load
        // fine and never play - the whole of issue #38.
        assert_eq!(mouse_button_for_definition("Escape"), "MouseLeft");
        assert_eq!(mouse_button_for_definition("Digit1"), "MouseRight");
        assert_eq!(mouse_button_for_definition("Digit2"), "MouseMiddle");
    }

    #[test]
    fn v2_definition_keys_pass_through_unchanged() {
        for button in ["MouseLeft", "MouseRight", "MouseMiddle"] {
            assert_eq!(mouse_button_for_definition(button), button);
        }
    }
}
