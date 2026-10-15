//! Import a skater shared as a `.cfss` file.
//!
//! A `.cfss` is a zip holding a Skate 3 (PS3) Create-a-Skater save,
//! `ALIAS_SKATER/SKATER.P`, its replacement textures,
//! `CHARACTER_TEXTURES/0x<texture id>.psg` (RenderWare PS3 textures), a
//! picture of the skater and a `config.json` with a title. The save's
//! `cas_db` names each slot's part, material and textures by the same 64-bit
//! ids as the customisation library, so a shared skater maps straight onto
//! the resident parts; the replacement textures stand in for the stock
//! diffuse texture of the same id, for this skater only.
//!
//! Everything runs where the game runs: the file is read from the player's
//! own disk (a file picker in the browser, a dropped file on the desktop) and
//! never sent anywhere. Decoded textures are kept as PNGs under
//! `settings/imported/`, named by their contents, beside the saved skater.
use crate::customiser_parts::Library;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The slots that may be left empty; a save without one wears none.
const OPTIONAL: [&str; 7] = ["Hat", "Hair", "Sock", "Glasses", "Jewellery", "WristItem", "Accessory"];
/// Imported textures are stored at up to this size (the stock ones are 512).
const MAX_TEXTURE: u32 = 1024;

pub(crate) struct Imported {
    pub title: String,
    pub profile: Value,
    /// Content hash and PNG bytes of each replacement texture.
    pub textures: Vec<(String, Vec<u8>)>,
    /// Parts the save names that this copy's library lacks.
    pub missing: usize,
}

fn infer_gender(cas: &Cas, library: &Library) -> Option<String> {
    let gender_for = |slot: &Slot| {
        library
            .models
            .get(&slot.asset)
            .filter(|part| part.slot == slot.slot)
            .map(|part| part.flag("Gender"))
            .filter(|gender| *gender == "male" || *gender == "female")
            .map(str::to_owned)
    };

    cas.slots
        .iter()
        .find(|slot| slot.slot == "Rostral")
        .and_then(gender_for)
        .or_else(|| cas.slots.iter().find_map(gender_for))
}

/// Read a `.cfss` and build a customiser profile from it on top of the
/// library's defaults for the skater's gender.
pub(crate) fn import(bytes: &[u8], library: &Library) -> Result<Imported, String> {
    let entries = zip_entries(bytes)?;
    let entry = |name: &str| entries.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, b)| b.as_slice());
    let save = entry("ALIAS_SKATER/SKATER.P").ok_or("This .cfss has no skater (ALIAS_SKATER/SKATER.P)")?;
    let title = entry("config.json")
        .and_then(|b| serde_json::from_slice::<Value>(&sanitize_json(b)).ok())
        .and_then(|v| v["title"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "Imported skater".into());
    let cas = parse_cas(save)?;

    let gender = infer_gender(&cas, library).unwrap_or_else(|| "male".into());
    let mut profile = library.defaults.get(&gender).cloned().ok_or("The character library has no default skater")?;
    profile["gender"] = json!(gender);

    // Parts and materials.
    let mut missing = 0;
    let mut worn: BTreeMap<String, (String, String)> = BTreeMap::new();
    for slot in &cas.slots {
        let Some(part) = library.models.get(&slot.asset).filter(|p| p.slot == slot.slot) else {
            missing += 1;
            continue;
        };
        let material = if part.materials.iter().any(|m| *m == slot.material) { slot.material.clone() } else { part.materials[0].clone() };
        profile["selections"][&slot.slot] = json!({"asset_id": slot.asset, "material_id": material});
        worn.insert(slot.slot.clone(), (slot.asset.clone(), material));
    }
    for slot in OPTIONAL {
        if !worn.contains_key(slot) && !profile["selections"][slot].is_null() {
            profile["selections"][slot] = Value::Null;
        }
    }
    if let Some(hair) = worn.get("Hair") {
        profile["hair_choice"] = json!({"asset_id": hair.0, "material_id": hair.1});
    }

    // Colours: the save remembers a colour per item ever coloured; only the
    // items worn take theirs. Skin tone comes from the head and arms.
    for colour in &cas.colours {
        let Some((slot, (_, material))) = worn.iter().find(|(_, (asset, _))| *asset == colour.asset) else { continue };
        if colour.rgb.iter().any(|c| !(0.0..=1.0).contains(c)) {
            continue;
        }
        let source = library.materials.get(material);
        if source.is_some_and(|m| !m.flag("SkinTone").is_empty()) {
            profile["skin_tint"] = json!(colour.rgb);
        } else if slot == "Hair" {
            profile["hair_tint"] = json!(colour.rgb);
        } else if source.is_some_and(|m| m.flag("IsColourizable").eq_ignore_ascii_case("true")) {
            profile["colours"][slot] = json!(colour.rgb);
        }
    }

    // Body and face shape: weight, definition and the face sliders, in the
    // library's order (the ethnicity blend is kept separately in the save).
    if !cas.morphs.is_empty() {
        let names: Vec<&String> = library.morphs.iter().filter(|m| !m.starts_with("global_")).collect();
        for (name, value) in names.iter().zip(&cas.morphs) {
            profile["morphs"][name.as_str()] = json!((f64::from(*value).clamp(0., 0.5) * 1000.).round() / 1000.);
        }
    }

    // Replacement textures for the diffuse of a worn material.
    let mut textures = Vec::new();
    let worn_diffuse: BTreeMap<String, ()> = worn
        .values()
        .filter_map(|(_, m)| library.materials.get(m))
        .map(|m| (m.diffuse_id(), ()))
        .collect();
    for (name, bytes) in &entries {
        let lower = name.to_lowercase();
        let Some(id) = lower.strip_prefix("character_textures/0x").and_then(|n| n.strip_suffix(".psg")) else { continue };
        if !worn_diffuse.contains_key(id) {
            continue;
        }
        let rgba = decode_psg(bytes).map_err(|e| format!("Texture {id}: {e}"))?;
        let png = encode_png(&rgba)?;
        let hash = format!("{:016x}", fnv1a(&png));
        profile["textures"][id] = json!(hash);
        textures.push((hash, png));
    }
    Ok(Imported { title, profile, textures, missing })
}

/// config.json in shared files holds raw line breaks inside strings.
fn sanitize_json(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    for &b in bytes {
        if in_string && !escaped && (b == b'\n' || b == b'\r') {
            out.extend_from_slice(if b == b'\n' { b"\\n" } else { b"" });
            continue;
        }
        if b == b'"' && !escaped {
            in_string = !in_string;
        }
        escaped = b == b'\\' && !escaped;
        out.push(b);
    }
    out
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

// ---- zip --------------------------------------------------------------------

fn le16(b: &[u8], at: usize) -> Result<usize, String> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize).ok_or_else(|| "Truncated zip".into())
}
fn le32(b: &[u8], at: usize) -> Result<usize, String> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize).ok_or_else(|| "Truncated zip".into())
}

/// Every file in a zip, by its central directory (stored or deflated).
fn zip_entries(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let search_from = bytes.len().saturating_sub(65_557);
    let end = (search_from..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| bytes[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or("This is not a valid Skate 3 .cfss save. Choose a shared-skater .cfss file.")?;
    let count = le16(bytes, end + 10)?;
    let mut at = le32(bytes, end + 16)?;
    let mut out = Vec::new();
    for _ in 0..count {
        if bytes.get(at..at + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err("Damaged .cfss (zip directory)".into());
        }
        let method = le16(bytes, at + 10)?;
        let compressed = le32(bytes, at + 20)?;
        let size = le32(bytes, at + 24)?;
        let (name_len, extra_len, comment_len) = (le16(bytes, at + 28)?, le16(bytes, at + 30)?, le16(bytes, at + 32)?);
        let local = le32(bytes, at + 42)?;
        let name = String::from_utf8_lossy(bytes.get(at + 46..at + 46 + name_len).ok_or("Truncated zip")?).replace('\\', "/");
        at += 46 + name_len + extra_len + comment_len;
        if bytes.get(local..local + 4) != Some(&[0x50, 0x4b, 0x03, 0x04]) {
            return Err("Damaged .cfss (zip entry)".into());
        }
        let data_at = local + 30 + le16(bytes, local + 26)? + le16(bytes, local + 28)?;
        let data = bytes.get(data_at..data_at + compressed).ok_or("Truncated .cfss")?;
        let data = match method {
            0 => data.to_vec(),
            8 => {
                use std::io::Read;
                let mut out = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).map_err(|e| format!("{name}: {e}"))?;
                out
            }
            other => return Err(format!("{name}: unsupported zip compression {other}")),
        };
        out.push((name, data));
    }
    Ok(out)
}

// ---- SKATER.P cas_db -----------------------------------------------------------

struct Slot {
    slot: String,
    asset: String,
    material: String,
}
struct Colour {
    rgb: [f32; 3],
    asset: String,
}
struct Cas {
    slots: Vec<Slot>,
    colours: Vec<Colour>,
    morphs: Vec<f32>,
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.b.get(self.at..self.at + n).ok_or("The skater save ends early")?;
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        if n > 256 {
            return Err("The skater save is not one this version reads".into());
        }
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn count(&mut self, limit: u32) -> Result<u32, String> {
        let n = self.u32()?;
        if n > limit {
            return Err("The skater save is not one this version reads".into());
        }
        Ok(n)
    }
}

/// The save's Create-a-Skater database: per slot the worn part and its
/// material (each with a few levels of detail naming their textures), then
/// the item colours, then the body and face shape.
fn parse_cas(save: &[u8]) -> Result<Cas, String> {
    let marker = save.windows(6).position(|w| w == b"cas_db").ok_or("No Create-a-Skater data in this save")?;
    let mut r = Reader { b: save, at: marker.checked_sub(4).ok_or("Damaged skater save")? };
    r.string()?;
    let _version = r.u32()?;
    r.u32()?;
    r.u32()?;
    let slot_count = r.count(64)?;
    let mut slots = Vec::new();
    for _ in 0..slot_count {
        let slot = r.string()?;
        let parts = r.count(16)?;
        for p in 0..parts {
            let asset = r.u64()?;
            let lods = r.count(8)?;
            let mut material = None;
            for _ in 0..lods {
                r.u64()?;
                r.u8()?;
                r.u64()?;
                r.u32()?;
                r.u32()?;
                let m = r.u64()?;
                material.get_or_insert(m);
                for _ in 0..r.count(32)? {
                    r.string()?;
                    r.u64()?;
                }
            }
            if p == 0 {
                if let Some(m) = material {
                    slots.push(Slot { slot: slot.clone(), asset: format!("{asset:016x}"), material: format!("{m:016x}") });
                }
            }
        }
    }
    // Colours: a flag, a section header, then indexed records of two RGB
    // tints and the item (part, material) they belong to.
    let mut colours = Vec::new();
    let colours_at = r.at;
    let parsed = (|| -> Result<(), String> {
        r.u8()?;
        r.u32()?;
        r.u8()?;
        r.u32()?;
        let n = r.count(512)?;
        for _ in 0..n {
            r.u32()?;
            let rgb = [r.f32()?, r.f32()?, r.f32()?];
            for _ in 0..3 {
                r.f32()?;
            }
            let asset = r.u64()?;
            r.u64()?;
            colours.push(Colour { rgb, asset: format!("{asset:016x}") });
        }
        Ok(())
    })();
    if parsed.is_err() {
        colours.clear();
        r.at = colours_at;
    }
    // Shape: a count of 19 to 23 values, each within the sliders' 0-1 range.
    let mut morphs = Vec::new();
    let window = r.at..(r.at + 512).min(save.len().saturating_sub(4));
    for at in window.step_by(1) {
        let n = u32::from_be_bytes(save[at..at + 4].try_into().unwrap()) as usize;
        if !(17..=23).contains(&n) || at + 4 + 4 * n > save.len() {
            continue;
        }
        let values: Vec<f32> = (0..n)
            .map(|i| f32::from_be_bytes(save[at + 4 + 4 * i..at + 8 + 4 * i].try_into().unwrap()))
            .collect();
        if values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) && values.iter().any(|v| *v > 0.) {
            morphs = values;
            break;
        }
    }
    Ok(Cas { slots, colours, morphs })
}

// ---- textures --------------------------------------------------------------------

pub(crate) struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// A RenderWare PS3 texture (`RW4ps3` .psg): its GCM texture descriptor
/// gives the format and size; the top mip level follows the header.
fn decode_psg(bytes: &[u8]) -> Result<Rgba, String> {
    if bytes.get(1..7) != Some(b"RW4ps3") {
        return Err("not a PS3 RenderWare texture".into());
    }
    let be16 = |at: usize| bytes.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]) as u32);
    let be32 = |at: usize| bytes.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
    // The descriptor sits after the arena's first object table: format
    // byte, mip count, then width and height (CellGcmTexture layout).
    let descriptor = (0x100..bytes.len().min(0x400) - 16)
        .step_by(4)
        .find(|&at| {
            let format = bytes[at] & !0x60;
            (0x81..=0x88).contains(&format)
                && bytes[at + 1] >= 1
                && bytes[at + 1] <= 13
                && be16(at + 8).is_some_and(|w| w.is_power_of_two() && (4..=4096).contains(&w))
                && be16(at + 10).is_some_and(|h| h.is_power_of_two() && (4..=4096).contains(&h))
        })
        .ok_or("no texture description")?;
    let format = bytes[descriptor] & !0x60;
    let linear = bytes[descriptor] & 0x20 != 0;
    let width = be16(descriptor + 8).unwrap();
    let height = be16(descriptor + 10).unwrap();
    let base_size = match format {
        0x86 => (width / 4).max(1) * (height / 4).max(1) * 8,
        0x87 | 0x88 => (width / 4).max(1) * (height / 4).max(1) * 16,
        0x85 => width * height * 4,
        _ => return Err(format!("texture format {format:#x} is not supported")),
    } as usize;
    // Pixel data starts at the arena's data offset (0x248 in the shared
    // files); fall back to the end of the file for one laid out otherwise.
    let data_at = be32(0x144)
        .filter(|&o| o >= 0x100 && o + base_size <= bytes.len())
        .ok_or("texture data out of range")?;
    let data = &bytes[data_at..data_at + base_size];
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    match format {
        0x85 => {
            if !linear {
                return Err("swizzled 32-bit textures are not supported".into());
            }
            for (i, px) in data.chunks_exact(4).enumerate() {
                // A8R8G8B8.
                pixels[i * 4..i * 4 + 4].copy_from_slice(&[px[1], px[2], px[3], px[0]]);
            }
        }
        _ => decode_dxt(format, data, width, height, &mut pixels),
    }
    let mut image = Rgba { width, height, pixels };
    while image.width > MAX_TEXTURE || image.height > MAX_TEXTURE {
        image = halve(&image);
    }
    Ok(image)
}

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]
}

/// DXT1/3/5 blocks, as the PS3 stores them: little-endian colour words.
fn decode_dxt(format: u8, data: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let block_size = if format == 0x86 { 8 } else { 16 };
    let (bw, bh) = ((width / 4).max(1), (height / 4).max(1));
    for by in 0..bh {
        for bx in 0..bw {
            let block = &data[((by * bw + bx) as usize) * block_size..][..block_size];
            let (alpha_block, colour) = if block_size == 16 { (&block[..8], &block[8..]) } else { (&block[..0], block) };
            let c0 = u16::from_le_bytes([colour[0], colour[1]]);
            let c1 = u16::from_le_bytes([colour[2], colour[3]]);
            let (a, b) = (rgb565(c0), rgb565(c1));
            let mut palette = [[0u8; 4]; 4];
            palette[0] = [a[0], a[1], a[2], 255];
            palette[1] = [b[0], b[1], b[2], 255];
            if c0 > c1 || block_size == 16 {
                for k in 0..3 {
                    palette[2][k] = ((2 * a[k] as u32 + b[k] as u32) / 3) as u8;
                    palette[3][k] = ((a[k] as u32 + 2 * b[k] as u32) / 3) as u8;
                }
                palette[2][3] = 255;
                palette[3][3] = 255;
            } else {
                for k in 0..3 {
                    palette[2][k] = ((a[k] as u32 + b[k] as u32) / 2) as u8;
                }
                palette[2][3] = 255;
                palette[3] = [0, 0, 0, 0];
            }
            let indices = u32::from_le_bytes([colour[4], colour[5], colour[6], colour[7]]);
            let mut alphas = [255u8; 16];
            match format {
                0x87 => {
                    for i in 0..16 {
                        let nibble = (alpha_block[i / 2] >> ((i % 2) * 4)) & 15;
                        alphas[i] = nibble * 17;
                    }
                }
                0x88 => {
                    let (a0, a1) = (alpha_block[0] as u32, alpha_block[1] as u32);
                    let mut table = [0u32; 8];
                    table[0] = a0;
                    table[1] = a1;
                    if a0 > a1 {
                        for k in 1..7 {
                            table[k + 1] = ((7 - k) as u32 * a0 + k as u32 * a1) / 7;
                        }
                    } else {
                        for k in 1..5 {
                            table[k + 1] = ((5 - k) as u32 * a0 + k as u32 * a1) / 5;
                        }
                        table[6] = 0;
                        table[7] = 255;
                    }
                    let bits = alpha_block[2..8].iter().rev().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
                    for i in 0..16 {
                        alphas[i] = table[((bits >> (3 * i)) & 7) as usize] as u8;
                    }
                }
                _ => {}
            }
            for i in 0..16u32 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x >= width || y >= height {
                    continue;
                }
                let mut px = palette[((indices >> (2 * i)) & 3) as usize];
                if block_size == 16 {
                    px[3] = alphas[i as usize];
                }
                let at = ((y * width + x) * 4) as usize;
                out[at..at + 4].copy_from_slice(&px);
            }
        }
    }
}

fn halve(image: &Rgba) -> Rgba {
    let (w, h) = ((image.width / 2).max(1), (image.height / 2).max(1));
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let sample = |dx: u32, dy: u32| {
                    let (sx, sy) = ((x * 2 + dx).min(image.width - 1), (y * 2 + dy).min(image.height - 1));
                    image.pixels[((sy * image.width + sx) * 4 + c) as usize] as u32
                };
                pixels[((y * w + x) * 4 + c) as usize] = ((sample(0, 0) + sample(1, 0) + sample(0, 1) + sample(1, 1) + 2) / 4) as u8;
            }
        }
    }
    Rgba { width: w, height: h, pixels }
}

fn encode_png(image: &Rgba) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&image.pixels, image.width, image.height, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Decode a stored imported texture (PNG) into an engine image.
pub(crate) fn image_from_png(png: &[u8]) -> Option<bevy::image::Image> {
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    let decoded = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.to_rgba8();
    let (width, height) = decoded.dimensions();
    Some(bevy::image::Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        decoded.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxt5_block_decodes_colours_and_alpha() {
        // One block: alpha endpoints 255/0, all indices 0 (opaque); colour
        // endpoints red/blue, all indices 1 (blue).
        let mut block = vec![255u8, 0, 0, 0, 0, 0, 0, 0];
        block.extend_from_slice(&0xf800u16.to_le_bytes());
        block.extend_from_slice(&0x001fu16.to_le_bytes());
        block.extend_from_slice(&0x5555_5555u32.to_le_bytes());
        let mut out = vec![0u8; 4 * 4 * 4];
        decode_dxt(0x88, &block, 4, 4, &mut out);
        assert_eq!(&out[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn config_titles_with_raw_line_breaks_parse() {
        let raw = b"{\"title\":\"a\",\"desc\":\"one\ntwo\"}";
        let v: Value = serde_json::from_slice(&sanitize_json(raw)).unwrap();
        assert_eq!(v["desc"], "one\ntwo");
    }

    #[test]
    fn non_cfss_file_suggests_choosing_a_shared_skater_save() {
        let error = import(b"not a file", &Library::default())
            .err()
            .expect("invalid input should fail");
        assert_eq!(
            error,
            "This is not a valid Skate 3 .cfss save. Choose a shared-skater .cfss file."
        );
    }

    #[test]
    fn gender_falls_back_to_another_known_slot_when_head_is_missing() {
        let part = |slot: &str, gender: &str| crate::customiser_parts::Part {
            slot: slot.into(),
            name: String::new(),
            flags: [("Gender".into(), gender.into())].into(),
            materials: Vec::new(),
            scene: String::new(),
        };
        let library = Library {
            models: [
                ("female_head".into(), part("Rostral", "female")),
                ("female_top".into(), part("OuterTorso", "female")),
            ]
            .into(),
            ..Default::default()
        };
        let cas = Cas {
            slots: vec![
                Slot {
                    slot: "Rostral".into(),
                    asset: "not_in_library".into(),
                    material: String::new(),
                },
                Slot {
                    slot: "OuterTorso".into(),
                    asset: "female_top".into(),
                    material: String::new(),
                },
            ],
            colours: Vec::new(),
            morphs: Vec::new(),
        };

        assert_eq!(infer_gender(&cas, &library).as_deref(), Some("female"));
    }

    /// A shared skater, when one is given (SKATE_CFSS=<path>), against the
    /// installed library (SKATE_LIBRARY=<library-v3.json>).
    #[test]
    #[ignore = "Requires a .cfss file and the private character library"]
    fn shared_skater_imports() {
        let bytes = std::fs::read(std::env::var("SKATE_CFSS").unwrap()).unwrap();
        let library: Library = serde_json::from_slice(&std::fs::read(std::env::var("SKATE_LIBRARY").unwrap()).unwrap()).unwrap();
        let imported = import(&bytes, &library).unwrap();
        let library: Library = serde_json::from_slice(&std::fs::read(std::env::var("SKATE_LIBRARY").unwrap()).unwrap()).unwrap();
        println!("{} ({} missing)", imported.title, imported.missing);
        println!("{}", serde_json::to_string_pretty(&imported.profile).unwrap());
        assert_eq!(imported.missing, 0);
        assert!(!imported.textures.is_empty());
        // The customiser's validation keeps the imported graphics.
        let mut parts = crate::customiser_parts::Parts::default();
        parts.library = library;
        let resolved = parts.resolve(&imported.profile).unwrap();
        assert_eq!(resolved["textures"], imported.profile["textures"]);
        assert_eq!(resolved["selections"]["OuterTorso"], imported.profile["selections"]["OuterTorso"]);
        if let Ok(dir) = std::env::var("SKATE_CFSS_OUT") {
            for (hash, png) in &imported.textures {
                std::fs::write(format!("{dir}/{hash}.png"), png).unwrap();
            }
        }
    }
}
