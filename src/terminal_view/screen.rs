use anyhow::{Result, ensure};
use std::{ffi::c_void, ptr::NonNull};

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Cell {
    text: [u8; 128],
    len: u32,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub flags: u16,
    pub width: u8,
}
impl Cell {
    pub fn text(&self) -> &str {
        if self.len == 0 {
            " "
        } else {
            std::str::from_utf8(&self.text[..self.len as usize]).unwrap_or("�")
        }
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    pub cursor: Option<(u16, u16)>,
}
impl Frame {
    pub fn text(&self) -> String {
        self.cells
            .chunks(self.columns as usize)
            .map(|row| {
                row.iter()
                    .filter(|cell| cell.width != 0)
                    .map(Cell::text)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

unsafe extern "C" {
    fn fluent_vt_new(columns: u16, rows: u16) -> *mut c_void;
    fn fluent_vt_free(vt: *mut c_void);
    fn fluent_vt_write(vt: *mut c_void, bytes: *const u8, len: usize);
    fn fluent_vt_replies(vt: *mut c_void, out: *mut u8) -> usize;
    fn fluent_vt_snapshot(
        vt: *mut c_void,
        cells: *mut Cell,
        capacity: usize,
        cursor: *mut u16,
    ) -> i32;
}

/// Own one libghostty-vt instance. Use it on a single thread; only copied cells
/// cross into dashboard rendering. No worker bytes reach the host terminal.
pub struct Screen {
    vt: NonNull<c_void>,
    columns: u16,
    rows: u16,
}
impl Screen {
    pub fn new(columns: u16, rows: u16) -> Result<Self> {
        dimensions(columns, rows)?;
        let vt = NonNull::new(unsafe { fluent_vt_new(columns, rows) })
            .ok_or_else(|| anyhow::anyhow!("allocating Ghostty terminal failed"))?;
        Ok(Self { vt, columns, rows })
    }
    pub fn write(&mut self, bytes: &[u8]) {
        unsafe {
            fluent_vt_write(self.vt.as_ptr(), bytes.as_ptr(), bytes.len());
        }
    }
    pub(crate) fn replies(&mut self) -> Vec<u8> {
        let mut buffer = [0u8; 4096];
        let len = unsafe { fluent_vt_replies(self.vt.as_ptr(), buffer.as_mut_ptr()) };
        buffer[..len].to_vec()
    }
    pub fn snapshot(&mut self) -> Result<Frame> {
        let count = self.columns as usize * self.rows as usize;
        // The C bridge initializes exactly `count` POD cells before success.
        let mut cells = Vec::<Cell>::with_capacity(count);
        let mut cursor = [0u16; 3];
        ensure!(
            unsafe {
                fluent_vt_snapshot(
                    self.vt.as_ptr(),
                    cells.as_mut_ptr(),
                    count,
                    cursor.as_mut_ptr(),
                )
            } == 0,
            "terminal display lost synchronization; reconnect to repaint"
        );
        unsafe {
            cells.set_len(count);
        }
        Ok(Frame {
            columns: self.columns,
            rows: self.rows,
            cells,
            cursor: (cursor[2] != 0).then_some((cursor[0], cursor[1])),
        })
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        unsafe {
            fluent_vt_free(self.vt.as_ptr());
        }
    }
}

pub(crate) fn dimensions(columns: u16, rows: u16) -> Result<()> {
    ensure!(
        columns > 0 && rows > 0 && columns <= 500 && rows <= 200,
        "terminal viewport must be between 1x1 and 500x200"
    );
    Ok(())
}
