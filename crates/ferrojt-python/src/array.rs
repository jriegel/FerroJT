//! The Python buffer protocol plumbing shared by the array types.
//!
//! Both [`JtArray`](lsg::JtArray) (the channels of one representation of a JT
//! file) and [`MeshBuffer`](mesh::MeshBuffer) (the channels of one flattened
//! mesh) hand their bytes to `memoryview` and NumPy **without a copy**: the
//! view points straight at the decoded or merged buffers and holds a reference
//! to the object that owns them, so the bytes cannot move while a view lives.
//!
//! The rules both types must follow — learned the hard way while writing the
//! file view — live here in one place:
//!
//! * an empty channel still hands out a **zero-length buffer** pointed at a
//!   static array. Returning `None` instead makes `numpy.asarray` fall back to
//!   a zero-dimensional **object** array rather than an empty `float32`.
//! * `shape`, `strides` and `format` must not point at anything on the stack of
//!   the call — the view outlives it. The shape/stride block and the format
//!   string are therefore heap allocations the exporter owns and frees again in
//!   `__releasebuffer__`.
//! * `__len__` is the **row count**, so `len(a)` agrees with
//!   `len(memoryview(a))` and `len(numpy.asarray(a))`.
//! * the view is always read-only; `PyBUF_WRITABLE` is refused.

use std::ffi::CString;
use std::ffi::{c_int, c_void};
use std::ptr;

use pyo3::exceptions::PyBufferError;
use pyo3::ffi;
use pyo3::prelude::*;

/// The three channels of a mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    Indices,
    Vertices,
    Normals,
}

impl Channel {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "indices" | "index" => Some(Channel::Indices),
            "vertices" | "positions" | "vertex" => Some(Channel::Vertices),
            "normals" | "normal" => Some(Channel::Normals),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Channel::Indices => "indices",
            Channel::Vertices => "vertices",
            Channel::Normals => "normals",
        }
    }

    /// All three channel names, in the order a buffer lists them.
    pub(crate) fn all() -> [Channel; 3] {
        [Channel::Indices, Channel::Vertices, Channel::Normals]
    }

    /// `struct` format code of one element.
    pub(crate) fn format(self) -> &'static str {
        match self {
            Channel::Indices => "i",
            Channel::Vertices | Channel::Normals => "f",
        }
    }

    /// Bytes per element of a channel: all three are 32 bit.
    pub(crate) fn itemsize(self) -> usize {
        4
    }
}

/// A resolved array: where the bytes are and how they are laid out.
pub(crate) struct Layout {
    pub(crate) data: *const u8,
    pub(crate) bytes: usize,
    pub(crate) itemsize: usize,
    pub(crate) format: &'static str,
    pub(crate) dims: Vec<usize>,
}

impl Layout {
    /// A layout over one slice of 32 bit values.
    ///
    /// An empty slice gets the stand-in address instead of the null pointer an
    /// empty `Vec` carries, because a buffer must always point at something
    /// even when its length is zero.
    pub(crate) fn of<T>(values: &[T], channel: Channel) -> Self {
        let count = values.len();
        let data = if count == 0 {
            &EMPTY as *const [u8; 4] as *const u8
        } else {
            values.as_ptr() as *const u8
        };
        Self {
            data,
            bytes: count * channel.itemsize(),
            itemsize: channel.itemsize(),
            format: channel.format(),
            dims: match channel {
                Channel::Indices => vec![count],
                Channel::Vertices | Channel::Normals => shaped(count),
            },
        }
    }
}

/// Stand-in address for a zero-length buffer.
static EMPTY: [u8; 4] = [0; 4];

/// A flat channel of `3 * n` floats as `(n, 3)`, flat when the count is not a
/// multiple of three.
pub(crate) fn shaped(count: usize) -> Vec<usize> {
    if count % 3 != 0 {
        return vec![count];
    }
    vec![count / 3, 3]
}

/// Fill a `Py_buffer` from a resolved layout.
///
/// # Safety
///
/// `view` must be a valid, non-null `Py_buffer`. `owner` must be an owned
/// reference that this call transfers into the view, and the bytes `layout`
/// points at must outlive the view.
pub(crate) unsafe fn fill_view(
    view: *mut ffi::Py_buffer,
    flags: c_int,
    layout: &Layout,
    owner: *mut ffi::PyObject,
) {
    let want_nd = flags & ffi::PyBUF_ND == ffi::PyBUF_ND;
    let want_strides = flags & ffi::PyBUF_STRIDES == ffi::PyBUF_STRIDES;
    let want_format = flags & ffi::PyBUF_FORMAT == ffi::PyBUF_FORMAT;

    // The shape and the strides have to stay alive for the whole life of the
    // view, so they go into one block this exporter owns and gives back in
    // `__releasebuffer__`. Layout: `[rows, cols, row stride, item size]`; the
    // shape is read from the first entry and the strides from the third, so a
    // one dimensional channel keeps `cols` at one and its row stride equals
    // the item size.
    let rows = layout.dims.first().copied().unwrap_or(0);
    let cols = layout.dims.get(1).copied().unwrap_or(1);
    let block = Box::into_raw(Box::new([
        rows as ffi::Py_ssize_t,
        cols as ffi::Py_ssize_t,
        (layout.itemsize * cols) as ffi::Py_ssize_t,
        layout.itemsize as ffi::Py_ssize_t,
    ]));
    let sizes = block as *mut ffi::Py_ssize_t;

    // SAFETY: `view` is valid, `sizes` points inside the leaked block above,
    // and `owner` is the reference this call is required to transfer.
    unsafe {
        (*view).buf = layout.data as *mut c_void;
        (*view).obj = owner;
        (*view).len = layout.bytes as ffi::Py_ssize_t;
        (*view).itemsize = layout.itemsize as ffi::Py_ssize_t;
        (*view).readonly = 1;
        (*view).ndim = if want_nd {
            layout.dims.len() as c_int
        } else {
            1
        };
        (*view).format = if want_format {
            CString::new(layout.format)
                .unwrap_or_else(|_| CString::new("").expect("the empty string has no NUL"))
                .into_raw()
        } else {
            ptr::null_mut()
        };
        (*view).shape = if want_nd { sizes } else { ptr::null_mut() };
        // Only the first entry of a strides array is read for a C ordered
        // array, which is what the channels are, so the row stride goes first.
        (*view).strides = if want_strides {
            sizes.add(2)
        } else {
            ptr::null_mut()
        };
        (*view).suboffsets = ptr::null_mut();
        (*view).internal = block as *mut c_void;
    }
}

/// Free what [`fill_view`] allocated for a view.
///
/// # Safety
///
/// `view` must be a `Py_buffer` that [`fill_view`] filled and that has not
/// been released yet.
pub(crate) unsafe fn release_view(view: *mut ffi::Py_buffer) {
    // SAFETY: `view` was filled by `fill_view`, which stores a leaked `CString`
    // in `format` and a leaked shape/stride block in `internal`.
    unsafe {
        if !(*view).format.is_null() {
            drop(CString::from_raw((*view).format));
            (*view).format = ptr::null_mut();
        }
        if !(*view).internal.is_null() {
            drop(Box::from_raw((*view).internal as *mut [ffi::Py_ssize_t; 4]));
            (*view).internal = ptr::null_mut();
        }
    }
}

/// The body of `__getbuffer__` for an owned array type.
///
/// `slf` is the object the view keeps alive; `layout` describes the bytes it
/// owns.
pub(crate) unsafe fn get_buffer<T>(
    slf: Bound<'_, T>,
    view: *mut ffi::Py_buffer,
    flags: c_int,
    resolved: Option<Layout>,
    read_only: &str,
) -> PyResult<()> {
    if view.is_null() {
        return Err(PyBufferError::new_err("the buffer view is null"));
    }
    if flags & ffi::PyBUF_WRITABLE == ffi::PyBUF_WRITABLE {
        return Err(PyBufferError::new_err(read_only.to_string()));
    }
    let Some(resolved) = resolved else {
        return Err(PyBufferError::new_err("the mesh has no geometry"));
    };
    let owner = slf.into_ptr();
    // SAFETY: `view` is a valid `Py_buffer` (checked above); the layout only
    // carries `Copy` values describing bytes that outlive the view, because
    // `owner` is the owned reference transferred into it and the buffers of
    // the object are never moved once it has been built.
    unsafe { fill_view(view, flags, &resolved, owner) };
    Ok(())
}
