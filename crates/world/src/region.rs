use crate::{Error, Tag, TileEntity, compound, publish};
use flate2::{
    Compression,
    read::{GzDecoder, ZlibDecoder},
    write::ZlibEncoder,
};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::Path,
};

const SECTOR: usize = 4096;

/// Replace one tile-entity record in an existing 1.8.9 Anvil chunk.
pub(crate) fn update_tile(world: &Path, tile: &TileEntity) -> Result<(), Error> {
    let [x, y, z] = tile.pos;
    let chunk_x = x.div_euclid(16);
    let chunk_z = z.div_euclid(16);
    let path = world.join("region").join(format!(
        "r.{}.{}.mca",
        chunk_x.div_euclid(32),
        chunk_z.div_euclid(32)
    ));
    let mut data = fs::read(&path)?;
    if data.len() < 2 * SECTOR || data.len() % SECTOR != 0 {
        return Err(Error::Setup("invalid Anvil region length".into()));
    }
    let index = (chunk_x.rem_euclid(32) + 32 * chunk_z.rem_euclid(32)) as usize;
    let loc = &data[index * 4..index * 4 + 4];
    let offset = (u32::from_be_bytes([0, loc[0], loc[1], loc[2]]) as usize) * SECTOR;
    let sectors = usize::from(loc[3]);
    if offset < 2 * SECTOR || sectors == 0 || offset + sectors * SECTOR > data.len() {
        return Err(Error::Setup(
            "tile entity requires an existing valid chunk".into(),
        ));
    }
    let length = u32::from_be_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| Error::Setup("invalid Anvil chunk header".into()))?,
    ) as usize;
    if length < 2 || length + 4 > sectors * SECTOR {
        return Err(Error::Setup("invalid Anvil chunk length".into()));
    }
    let payload = &data[offset + 5..offset + 4 + length];
    let mut raw = Vec::new();
    match data[offset + 4] {
        1 => GzDecoder::new(payload)
            .take(16 * 1024 * 1024)
            .read_to_end(&mut raw)?,
        2 => ZlibDecoder::new(payload)
            .take(16 * 1024 * 1024)
            .read_to_end(&mut raw)?,
        _ => return Err(Error::Setup("unsupported Anvil chunk compression".into())),
    };
    let mut chunk: HashMap<String, Tag> = fastnbt::from_bytes(&raw)?;
    let level = compound(&mut chunk, "Level")?;
    let values = level
        .entry("TileEntities".into())
        .or_insert_with(|| Tag::List(Vec::new()));
    let Tag::List(values) = values else {
        return Err(Error::Setup("invalid chunk TileEntities".into()));
    };
    values.retain(|value| {
        let Tag::Compound(fields) = value else {
            return true;
        };
        [("x", x), ("y", y), ("z", z)]
            .iter()
            .any(|(key, coordinate)| fields.get(*key) != Some(&Tag::Int(*coordinate)))
    });
    let mut fields = tile.fields.clone();
    fields.extend([
        ("id".into(), Tag::String(tile.id.clone())),
        ("x".into(), Tag::Int(x)),
        ("y".into(), Tag::Int(y)),
        ("z".into(), Tag::Int(z)),
    ]);
    values.push(Tag::Compound(fields));
    let mut compressed = ZlibEncoder::new(Vec::new(), Compression::default());
    compressed.write_all(&fastnbt::to_bytes(&chunk)?)?;
    let compressed = compressed.finish()?;
    let count = (compressed.len() + 5).div_ceil(SECTOR);
    let target = if count <= sectors { offset } else { data.len() };
    if count > 255 || target / SECTOR > 0x00ff_ffff {
        return Err(Error::Setup(
            "updated chunk exceeds Anvil region limits".into(),
        ));
    }
    let end = target + count * SECTOR;
    if end > data.len() {
        data.resize(end, 0);
    }
    let location = (((target / SECTOR) as u32) << 8) | count as u32;
    data[index * 4..index * 4 + 4].copy_from_slice(&location.to_be_bytes());
    let record_length = (compressed.len() as u32 + 1).to_be_bytes();
    data[target..target + 4].copy_from_slice(&record_length);
    data[target + 4] = 2;
    data[target + 5..target + 5 + compressed.len()].copy_from_slice(&compressed);
    data[target + 5 + compressed.len()..end].fill(0);
    publish(&path, &data)
}
