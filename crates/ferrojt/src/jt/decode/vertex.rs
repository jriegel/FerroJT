//! Vertex coordinate and normal payloads.
//!
//! JT stores the numeric arrays of a shape LOD as a small set of integer codec
//! packages plus a description of how to turn the decoded integers into
//! floating point values. Four schemes exist:
//!
//! | scheme     | packages per component | used by                      |
//! |------------|------------------------|------------------------------|
//! | quantized  | 1                      | lossy vertex data            |
//! | deering    | 4 (9.x) / 1 (10.x)     | lossy vertex normals         |
//! | exp/mant   | 2                      | lossless data before JT 10   |
//! | raw float  | 1                      | lossless data from JT 10 on  |
//!
//! The declared element count is available through [`VertexData::out_vertex_count`]
//! and must be read *before* [`VertexData::decode`], which consumes the packages.

use std::sync::OnceLock;

use super::cdp::Package;
use super::unpack::Unpack;
use crate::codec::ByteOrder;
use crate::error::{Error, Result};
use crate::jt::reader::Cursor;

/// Number of index bits in the Deering normal lookup table.
const TABLE_BITS: u32 = 13;
/// Size of the Deering lookup table.
const TABLE_SIZE: usize = 1 << TABLE_BITS;
/// Extra entries kept past the table end by the reference implementation.
const TABLE_TAIL: usize = 16;
/// Maximum psi angle of the Deering parameterisation.
const MAX_PSI: f64 = 0.615_479_709;

/// Per-axis quantization parameters (9 bytes on disk).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UniformQuantizer {
    /// Lower bound of the axis range.
    pub min: f32,
    /// Upper bound of the axis range.
    pub max: f32,
    /// Number of code bits used for the axis.
    pub bits: u8,
}

impl UniformQuantizer {
    /// Reads the three fields from `cursor`.
    pub fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        Ok(Self {
            min: cursor.read_f32()?,
            max: cursor.read_f32()?,
            bits: cursor.read_u8()?,
        })
    }
}

/// Per-axis quantization parameters of a vertex position (27 bytes on disk).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointQuantizer {
    /// X axis parameters.
    pub x: UniformQuantizer,
    /// Y axis parameters.
    pub y: UniformQuantizer,
    /// Z axis parameters.
    pub z: UniformQuantizer,
}

impl PointQuantizer {
    /// Reads the three axes from `cursor`.
    pub fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        Ok(Self {
            x: UniformQuantizer::read(cursor)?,
            y: UniformQuantizer::read(cursor)?,
            z: UniformQuantizer::read(cursor)?,
        })
    }

    /// The parameters of component `index`, X/Y/Z.
    #[must_use]
    pub fn component(&self, index: usize) -> UniformQuantizer {
        match index {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }
}

/// How the packages of a [`VertexData`] turn into floats.
#[derive(Clone, Copy, Debug)]
enum Scheme {
    /// Uniform quantization of each component.
    Quantized,
    /// Spherical parameterisation of unit normals.
    Deering {
        /// Code width of each packed field.
        bits: u8,
        /// Whether the JT 10.x single-package packing is used.
        packed: bool,
    },
    /// Lossless exponent / mantissa split.
    ExpMant,
    /// Lossless raw IEEE-754 words.
    RawFloat,
}

/// A loaded vertex payload, ready to be decoded.
#[derive(Clone, Debug)]
pub struct VertexData {
    packages: Vec<Package>,
    unpack: Unpack,
    scheme: Scheme,
    quantizer: PointQuantizer,
}

impl VertexData {
    /// Declared number of elements, valid until [`VertexData::decode`] runs.
    #[must_use]
    pub fn out_vertex_count(&self) -> usize {
        self.packages.first().map_or(0, Package::out_val_count)
    }

    /// Number of float components per element.
    #[must_use]
    pub fn out_comp_count(&self) -> usize {
        match self.scheme {
            Scheme::Quantized | Scheme::RawFloat => self.packages.len(),
            Scheme::Deering { .. } => 3,
            Scheme::ExpMant => self.packages.len() / 2,
        }
    }

    /// Loads lossy quantized coordinates (mark 1 packages).
    pub fn load_quantized_coords(cursor: &mut Cursor<'_>) -> Result<Self> {
        let quantizer = PointQuantizer::read(cursor)?;
        let count = read_count(cursor, "vertex")?;
        let mut data = Self {
            packages: vec![Package::empty(); 3],
            unpack: Unpack::Lag1,
            scheme: Scheme::Quantized,
            quantizer,
        };
        data.load(cursor, true, count)?;
        Ok(data)
    }

    /// Loads lossless or quantized coordinates (mark 2 packages).
    pub fn load_compressed_coords(cursor: &mut Cursor<'_>) -> Result<Self> {
        let count = read_count(cursor, "vertex")?;
        let components = usize::from(cursor.read_u8()?);
        let quantizer = PointQuantizer::read(cursor)?;
        let scheme = if quantizer.x.bits != 0 {
            Scheme::Quantized
        } else if cursor.major() >= 10 {
            Scheme::RawFloat
        } else {
            Scheme::ExpMant
        };
        let packages = package_count(scheme, components);
        let mut data = Self {
            packages: vec![Package::empty(); packages],
            unpack: Unpack::Lag1,
            scheme,
            quantizer,
        };
        data.load(cursor, false, count)?;
        // A trailing hash is stored but never verified.
        let _hash = cursor.read_i32()?;
        Ok(data)
    }

    /// Loads lossy quantized normals (mark 1 packages).
    pub fn load_quantized_normals(cursor: &mut Cursor<'_>) -> Result<Self> {
        let bits = cursor.read_u8()?;
        let count = read_count(cursor, "normal")?;
        let mut data = Self {
            packages: vec![Package::empty(); 4],
            unpack: Unpack::Lag1,
            scheme: Scheme::Deering {
                bits,
                packed: false,
            },
            quantizer: PointQuantizer::default(),
        };
        data.load(cursor, true, count)?;
        Ok(data)
    }

    /// Loads lossless or quantized normals (mark 2 packages).
    pub fn load_compressed_normals(cursor: &mut Cursor<'_>) -> Result<Self> {
        let count = read_count(cursor, "normal")?;
        let components = usize::from(cursor.read_u8()?);
        let bits = cursor.read_u8()?;
        let scheme = if bits != 0 {
            Scheme::Deering {
                bits,
                packed: cursor.major() >= 10,
            }
        } else if cursor.major() >= 10 {
            Scheme::RawFloat
        } else {
            Scheme::ExpMant
        };
        let packages = package_count(scheme, components);
        let mut data = Self {
            packages: vec![Package::empty(); packages],
            unpack: Unpack::Null,
            scheme,
            quantizer: PointQuantizer::default(),
        };
        data.load(cursor, false, count)?;
        // A trailing hash is stored but never verified.
        let _hash = cursor.read_i32()?;
        Ok(data)
    }

    /// Reads every package and checks that each one declares `count` values.
    fn load(&mut self, cursor: &mut Cursor<'_>, mark1: bool, count: usize) -> Result<()> {
        for package in &mut self.packages {
            *package = if mark1 {
                Package::load1(cursor)?
            } else {
                Package::load2(cursor)?
            };
            if package.out_val_count() != count {
                return Err(cursor.error(format!(
                    "vertex data package holds {} values, expected {count}",
                    package.out_val_count()
                )));
            }
        }
        Ok(())
    }

    /// Decodes the payload into `comp_count` floats per element.
    ///
    /// The packages are consumed by this call.
    pub fn decode(&mut self, order: ByteOrder) -> Result<Decoded> {
        let count = self.out_vertex_count();
        let comps = self.out_comp_count();
        let unpack = self.unpack;
        let quantizer = self.quantizer;
        let mut data = vec![0.0f32; count * comps];
        match self.scheme {
            Scheme::Quantized => {
                if comps > 3 {
                    return Err(Error::corrupt(format!(
                        "quantized vertex data has {comps} components, more than the three axes"
                    )));
                }
                for j in 0..comps {
                    let codes = self.packages[j].decode(order, unpack)?;
                    let quant = quantizer.component(j);
                    let min = f64::from(quant.min);
                    let max = f64::from(quant.max);
                    let max_code = if u32::from(quant.bits) < 32 {
                        f64::from(1u32 << u32::from(quant.bits))
                    } else {
                        4_294_967_295.0
                    };
                    let multiplier = (max - min) / max_code;
                    for (i, code) in codes.iter().enumerate() {
                        // The code is reinterpreted as unsigned and moved by
                        // half a cell to land in the middle of its interval.
                        let value = min + (f64::from(*code as u32) - 0.5) * multiplier;
                        data[i * comps + j] = value as f32;
                    }
                }
            }
            Scheme::RawFloat => {
                for j in 0..comps {
                    let codes = self.packages[j].decode(order, unpack)?;
                    for (i, code) in codes.iter().enumerate() {
                        data[i * comps + j] = f32::from_bits(*code as u32);
                    }
                }
            }
            Scheme::ExpMant => {
                for j in 0..comps {
                    let exponent = self.packages[j * 2].decode(order, unpack)?;
                    let mantissa = self.packages[j * 2 + 1].decode(order, unpack)?;
                    for (i, (exponent, mantissa)) in
                        exponent.iter().zip(mantissa.iter()).enumerate()
                    {
                        let bits = (*exponent as u32).wrapping_shl(23) | (*mantissa as u32);
                        data[i * comps + j] = f32::from_bits(bits);
                    }
                }
            }
            Scheme::Deering { bits, packed } => {
                if u32::from(bits) > TABLE_BITS {
                    return Err(Error::corrupt(format!(
                        "normal encoding uses {bits} code bits, more than the {TABLE_BITS} the \
                         lookup table provides"
                    )));
                }
                let table = lookup_table();
                let width = u32::from(bits);
                let mask = (1u32 << width) - 1;
                let offset = TABLE_BITS - width;
                let decoded = if packed {
                    vec![self.packages[0].decode(order, unpack)?]
                } else {
                    let mut parts = Vec::with_capacity(self.packages.len());
                    for package in &mut self.packages {
                        parts.push(package.decode(order, unpack)?);
                    }
                    parts
                };
                for i in 0..count {
                    let (sextant, octant, theta_code, psi_code) = if packed {
                        let code = decoded[0][i] as u32;
                        (
                            (code >> (2 * width + 3)) & 7,
                            (code >> (2 * width)) & 7,
                            (code >> width) & mask,
                            code & mask,
                        )
                    } else {
                        (
                            decoded[0][i] as u32,
                            decoded[1][i] as u32,
                            decoded[2][i] as u32,
                            decoded[3][i] as u32,
                        )
                    };
                    let theta = table
                        .theta
                        .get(usize::try_from((theta_code + (sextant & 1)) << offset).unwrap_or(0))
                        .copied()
                        .unwrap_or_default();
                    let psi = table
                        .psi
                        .get(usize::try_from(psi_code << offset).unwrap_or(0))
                        .copied()
                        .unwrap_or_default();

                    let x = (psi.cos * theta.cos) as f32;
                    let y = psi.sin as f32;
                    let z = (psi.cos * theta.sin) as f32;
                    let mut result = match sextant {
                        1 => [z, y, x],
                        2 => [y, z, x],
                        3 => [y, x, z],
                        4 => [z, x, y],
                        5 => [x, z, y],
                        _ => [x, y, z],
                    };
                    // A clear bit negates the matching axis.
                    if octant & 4 == 0 {
                        result[0] = -result[0];
                    }
                    if octant & 2 == 0 {
                        result[1] = -result[1];
                    }
                    if octant & 1 == 0 {
                        result[2] = -result[2];
                    }
                    data[i * 3..i * 3 + 3].copy_from_slice(&result);
                }
            }
        }
        Ok(Decoded {
            count,
            comp_count: comps,
            data,
        })
    }
}

/// Number of packages a scheme needs for `components` output components.
fn package_count(scheme: Scheme, components: usize) -> usize {
    match scheme {
        Scheme::ExpMant => components * 2,
        Scheme::Deering { packed: true, .. } => 1,
        Scheme::Deering { .. } => 4,
        _ => components,
    }
}

/// Reads a non-negative element count.
fn read_count(cursor: &mut Cursor<'_>, what: &str) -> Result<usize> {
    let raw = cursor.read_i32()?;
    if raw < 0 {
        return Err(cursor.error(format!("negative {what} count {raw}")));
    }
    usize::try_from(raw).map_err(|_| cursor.error(format!("{what} count {raw} is too large")))
}

/// A sine/cosine pair from the Deering lookup table.
#[derive(Clone, Copy, Debug, Default)]
struct SinCos {
    sin: f64,
    cos: f64,
}

impl SinCos {
    fn of(angle: f64) -> Self {
        Self {
            sin: angle.sin(),
            cos: angle.cos(),
        }
    }
}

/// The Deering theta/psi lookup table.
struct LookupTable {
    theta: Vec<SinCos>,
    psi: Vec<SinCos>,
}

/// Phi values used past the end of the regular table.
const PHI: [f64; TABLE_TAIL] = [
    0.904_837, 0.818_731, 0.740_818, 0.670_320, 0.606_531, 0.548_812, 0.449_329, 0.367_879,
    0.223_13, 0.135_335, 0.090_484, 0.163_746, 0.222_245, 0.268_128, 0.303_265, 0.329_287,
];

/// Builds the Deering lookup table.
fn build_lookup_table() -> LookupTable {
    let size = TABLE_SIZE + TABLE_TAIL;
    let mut theta = Vec::with_capacity(size);
    let mut psi = Vec::with_capacity(size);
    let table_size = f64::from(u32::try_from(TABLE_SIZE).expect("table size fits u32"));
    for index in 0..=TABLE_SIZE {
        let back = f64::from(u32::try_from(TABLE_SIZE - index).expect("fits u32"));
        theta.push(SinCos::of((MAX_PSI * back / table_size).tan().asin()));
        psi.push(SinCos::of(
            MAX_PSI * (f64::from(u32::try_from(index).expect("fits u32")) / table_size),
        ));
    }
    // The reference fills only TABLE_SIZE + 15 regular entries plus the tail,
    // so the last Phi value is never actually stored.
    for phi in &PHI[..TABLE_TAIL - 1] {
        theta.push(SinCos::of(phi.tan().asin()));
        psi.push(SinCos::of(*phi));
    }
    LookupTable { theta, psi }
}

/// The Deering lookup table, built on first use.
fn lookup_table() -> &'static LookupTable {
    static TABLE: OnceLock<LookupTable> = OnceLock::new();
    TABLE.get_or_init(build_lookup_table)
}

/// Decoded vertex data: `comp_count` floats per element, row major.
#[derive(Clone, Debug, Default)]
pub struct Decoded {
    count: usize,
    comp_count: usize,
    data: Vec<f32>,
}

impl Decoded {
    /// Number of decoded elements.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    /// Number of float components per element.
    #[must_use]
    pub fn comp_count(&self) -> usize {
        self.comp_count
    }

    /// The component `j` of element `i`.
    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f32 {
        self.data[i * self.comp_count + j]
    }

    /// The raw values, row major.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Appends a mark 2 package holding `words` verbatim.
    fn null_package2(words: &[u32], out: &mut Vec<u8>) {
        out.extend_from_slice(&(words.len() as i32).to_le_bytes());
        out.push(0); // null codec
        out.extend_from_slice(&((words.len() * 32) as i32).to_le_bytes());
        for word in words {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }

    #[test]
    fn quantizer_reads_twenty_seven_bytes() {
        let mut data = Vec::new();
        for axis in 0..3u8 {
            data.extend_from_slice(&(-1.0f32).to_le_bytes());
            data.extend_from_slice(&(1.0f32).to_le_bytes());
            data.push(10 + axis);
        }
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let quantizer = PointQuantizer::read(&mut cursor).expect("read");
        assert_eq!(quantizer.x.min, -1.0);
        assert_eq!(quantizer.z.bits, 12);
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn raw_float_coords_round_trip() {
        let mut data = Vec::new();
        data.extend_from_slice(&2i32.to_le_bytes()); // element count
        data.push(3); // components
        for _ in 0..3 {
            data.extend_from_slice(&(-1.0f32).to_le_bytes());
            data.extend_from_slice(&(1.0f32).to_le_bytes());
            data.push(0); // no quantization -> raw floats
        }
        for axis in 0..3u32 {
            null_package2(
                &[(axis as f32).to_bits(), (axis as f32 + 1.0).to_bits()],
                &mut data,
            );
        }
        data.extend_from_slice(&0i32.to_le_bytes()); // hash
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 10);
        let mut vertices = VertexData::load_compressed_coords(&mut cursor).expect("load");
        assert_eq!(vertices.out_vertex_count(), 2);
        assert_eq!(vertices.out_comp_count(), 3);
        let decoded = vertices.decode(ByteOrder::Little).expect("decode");
        assert_eq!(decoded.get(0, 0), 0.0);
        assert_eq!(decoded.get(1, 0), 1.0);
        assert_eq!(decoded.get(1, 2), 3.0);
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn quantized_coords_use_the_cell_centre() {
        let mut data = Vec::new();
        data.extend_from_slice(&2i32.to_le_bytes()); // element count
        data.push(3); // components
        for _ in 0..3 {
            data.extend_from_slice(&(-1.0f32).to_le_bytes());
            data.extend_from_slice(&(1.0f32).to_le_bytes());
            data.push(2); // two code bits
        }
        for _ in 0..3 {
            null_package2(&[0, 3], &mut data);
        }
        data.extend_from_slice(&0i32.to_le_bytes()); // hash
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 10);
        let mut vertices = VertexData::load_compressed_coords(&mut cursor).expect("load");
        let decoded = vertices.decode(ByteOrder::Little).expect("decode");
        // min + (code - 0.5) * (max - min) / 4
        assert_eq!(decoded.get(0, 0), -1.25);
        assert_eq!(decoded.get(1, 2), 0.25);
    }

    #[test]
    fn exp_mant_reassembles_the_float() {
        let mut data = Vec::new();
        data.extend_from_slice(&1i32.to_le_bytes()); // element count
        data.push(1); // components
        for _ in 0..3 {
            data.extend_from_slice(&0.0f32.to_le_bytes());
            data.extend_from_slice(&0.0f32.to_le_bytes());
            data.push(0); // no quantization
        }
        // 2.5 = 0x40200000 -> exponent field 0x80, mantissa 0x200000.
        null_package2(&[0x80], &mut data);
        null_package2(&[0x0020_0000], &mut data);
        data.extend_from_slice(&0i32.to_le_bytes()); // hash
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut vertices = VertexData::load_compressed_coords(&mut cursor).expect("load");
        assert_eq!(vertices.out_comp_count(), 1);
        let decoded = vertices.decode(ByteOrder::Little).expect("decode");
        assert_eq!(decoded.get(0, 0), 2.5);
    }

    #[test]
    fn mismatched_package_size_is_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(&2i32.to_le_bytes()); // element count
        data.push(3); // components
        for _ in 0..3 {
            data.extend_from_slice(&0.0f32.to_le_bytes());
            data.extend_from_slice(&0.0f32.to_le_bytes());
            data.push(0);
        }
        null_package2(&[0, 0], &mut data);
        null_package2(&[0], &mut data);
        null_package2(&[0, 0], &mut data);
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 10);
        let error = VertexData::load_compressed_coords(&mut cursor)
            .expect_err("the second package is short");
        assert!(matches!(error, Error::Invalid { .. }), "got {error:?}");
    }

    #[test]
    fn deering_rejects_codes_wider_than_the_table() {
        let mut data = vec![20u8]; // bits
        data.extend_from_slice(&0i32.to_le_bytes()); // element count
        for _ in 0..4 {
            data.push(0); // null codec
            data.extend_from_slice(&0i32.to_le_bytes()); // no words
        }
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut normals = VertexData::load_quantized_normals(&mut cursor).expect("load");
        let error = normals
            .decode(ByteOrder::Little)
            .expect_err("the table cannot cover 20 bits");
        assert!(matches!(error, Error::Corrupt(_)), "got {error:?}");
    }

    #[test]
    fn deering_maps_the_neutral_code_to_a_unit_axis() {
        // theta code 0 and psi code 0 sit at the top of the parameterisation.
        let mut data = vec![9u8]; // bits
        data.extend_from_slice(&1i32.to_le_bytes()); // element count
        for _ in 0..4 {
            data.push(0); // null codec
            data.extend_from_slice(&1i32.to_le_bytes()); // one word
            data.extend_from_slice(&0i32.to_le_bytes()); // sextant/octant/codes all zero
        }
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut normals = VertexData::load_quantized_normals(&mut cursor).expect("load");
        assert_eq!(normals.out_comp_count(), 3);
        let decoded = normals.decode(ByteOrder::Little).expect("decode");
        // Theta[0] is asin(tan(MAX_PSI)) = pi/4, Psi[0] is 0, so the raw vector
        // is (sqrt(2)/2, 0, sqrt(2)/2). Every octant bit is clear, which
        // negates all three axes.
        let root = f64::sqrt(0.5);
        assert!((f64::from(decoded.get(0, 0)) + root).abs() < 1e-6);
        assert_eq!(decoded.get(0, 1), 0.0);
        assert!((f64::from(decoded.get(0, 2)) + root).abs() < 1e-6);
    }
}
