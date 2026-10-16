//! Bounded, typed encoding for one retired physical terminal row.
//!
//! Format `PIGROW02` is little-endian: header, pooled links, then attribute-
//! delta cells with UTF-8 scalar values and optional extras.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};

use alacritty_terminal::{
    grid::Row,
    term::cell::{Cell, Flags, Hyperlink},
    vte::ansi::{Color, NamedColor, Rgb},
};
use anyhow::{Result, anyhow, bail, ensure};

pub const MAX_ROW_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LINK_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_COLUMNS: usize = 1024;
pub const MAX_ZERO_WIDTH_CHARS: usize = 1024 * 1024;
const MAGIC: &[u8; 8] = b"PIGROW02";
const VALID_FLAGS: u16 = (1 << 15) - 1;
const FG_CHANGED: u8 = 1 << 0;
const BG_CHANGED: u8 = 1 << 1;
const FLAGS_CHANGED: u8 = 1 << 2;
const UNDERLINE_CHANGED: u8 = 1 << 3;
const LINK_CHANGED: u8 = 1 << 4;
const ZERO_WIDTH_PRESENT: u8 = 1 << 5;
const CHAR_SAME: u8 = 1 << 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    pub zerowidth: Vec<char>,
    pub underline_color: Option<Color>,
    /// Index into `DecodedRow::hyperlinks`.
    pub hyperlink: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedHyperlink {
    pub id: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRow {
    pub columns: u16,
    pub occupied: u16,
    pub hyperlinks: Vec<DecodedHyperlink>,
    pub cells: Vec<DecodedCell>,
}

struct BoundedWriter<'a, W> {
    inner: &'a mut W,
    written: usize,
}
impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .written
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("row size overflow"))?;
        if end > MAX_ROW_BYTES {
            return Err(io::Error::other("encoded row exceeds 16 MiB"));
        }
        self.inner.write_all(bytes)?;
        self.written = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
fn put_u16(w: &mut impl Write, v: u16) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn put_u32(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn put_string(w: &mut impl Write, s: &str) -> io::Result<()> {
    let len = u32::try_from(s.len()).map_err(io::Error::other)?;
    put_u32(w, len)?;
    w.write_all(s.as_bytes())
}

fn put_char(w: &mut impl Write, c: char) -> io::Result<()> {
    let mut buf = [0; 4];
    w.write_all(c.encode_utf8(&mut buf).as_bytes())
}

fn encode_color(w: &mut impl Write, c: Color) -> io::Result<()> {
    match c {
        Color::Named(n) => {
            w.write_all(&[0])?;
            put_u16(w, n as u16)
        }
        Color::Spec(Rgb { r, g, b }) => w.write_all(&[1, r, g, b]),
        Color::Indexed(i) => w.write_all(&[2, i]),
    }
}

/// Stream one complete row. Link strings are pooled by their `(id, uri)` content
/// and table entries borrow cells, so large URI data is never cloned or repeated.
pub fn encode_row(row: &Row<Cell>, writer: &mut impl Write) -> Result<()> {
    let columns = row.len();
    ensure!(
        (1..=MAX_COLUMNS).contains(&columns),
        "row column count outside bounds"
    );
    ensure!(row.occupied() <= columns, "occupied bound exceeds columns");
    let mut links: Vec<Hyperlink> = Vec::new();
    let mut identity: HashMap<(usize, usize), u16> = HashMap::new();
    let mut content: HashMap<u64, Vec<u16>> = HashMap::new();
    let mut link_bytes = 0usize;
    for cell in row {
        if let Some(link) = cell.hyperlink() {
            let identity_key = (link.id().as_ptr() as usize, link.uri().as_ptr() as usize);
            if identity.contains_key(&identity_key) {
                continue;
            }
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            link.id().hash(&mut hasher);
            link.uri().hash(&mut hasher);
            let digest = hasher.finish();
            let duplicate = content.get(&digest).and_then(|indices| {
                indices.iter().copied().find(|ix| {
                    let known = &links[*ix as usize];
                    known.id() == link.id() && known.uri() == link.uri()
                })
            });
            if let Some(ix) = duplicate {
                identity.insert(identity_key, ix);
            } else {
                ensure!(links.len() < u16::MAX as usize, "too many row hyperlinks");
                link_bytes = link_bytes
                    .checked_add(link.id().len())
                    .and_then(|n| n.checked_add(link.uri().len()))
                    .ok_or_else(|| anyhow!("link byte count overflow"))?;
                ensure!(
                    link_bytes <= MAX_LINK_BYTES,
                    "row hyperlink pool exceeds 2 MiB"
                );
                let ix = links.len() as u16;
                links.push(link);
                identity.insert(identity_key, ix);
                content.entry(digest).or_default().push(ix);
            }
        }
    }
    let mut out = BoundedWriter {
        inner: writer,
        written: 0,
    };
    out.write_all(MAGIC)?;
    put_u16(&mut out, columns as u16)?;
    put_u16(&mut out, row.occupied() as u16)?;
    put_u16(&mut out, links.len() as u16)?;
    for link in &links {
        put_string(&mut out, link.id())?;
        put_string(&mut out, link.uri())?;
    }
    let mut previous_char = ' ';
    let mut previous_fg = Color::Named(NamedColor::Foreground);
    let mut previous_bg = Color::Named(NamedColor::Background);
    let mut previous_flags = Flags::empty();
    let mut previous_underline = None;
    let mut previous_link = None;
    let mut zero_total = 0usize;
    for cell in row {
        let flags = cell.flags.bits();
        ensure!(flags & !VALID_FLAGS == 0, "invalid cell flags");
        let zw = cell.zerowidth().unwrap_or(&[]);
        zero_total = zero_total
            .checked_add(zw.len())
            .ok_or_else(|| anyhow!("zerowidth count overflow"))?;
        ensure!(
            zero_total <= MAX_ZERO_WIDTH_CHARS,
            "row zerowidth character limit exceeded"
        );
        let link = cell.hyperlink();
        let link_index = match link {
            None => None,
            Some(ref link) => {
                let key = (link.id().as_ptr() as usize, link.uri().as_ptr() as usize);
                let ix = identity
                    .get(&key)
                    .copied()
                    .or_else(|| {
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        link.id().hash(&mut hasher);
                        link.uri().hash(&mut hasher);
                        content.get(&hasher.finish()).and_then(|indices| {
                            indices.iter().copied().find(|ix| {
                                let known = &links[*ix as usize];
                                known.id() == link.id() && known.uri() == link.uri()
                            })
                        })
                    })
                    .ok_or_else(|| anyhow!("hyperlink pool invariant"))?;
                Some(ix)
            }
        };
        let underline = cell.underline_color();
        let mut control = 0;
        if cell.fg != previous_fg {
            control |= FG_CHANGED;
        }
        if cell.bg != previous_bg {
            control |= BG_CHANGED;
        }
        if cell.flags != previous_flags {
            control |= FLAGS_CHANGED;
        }
        if underline != previous_underline {
            control |= UNDERLINE_CHANGED;
        }
        if link_index != previous_link {
            control |= LINK_CHANGED;
        }
        if !zw.is_empty() {
            control |= ZERO_WIDTH_PRESENT;
        }
        if cell.c == previous_char {
            control |= CHAR_SAME;
        }
        out.write_all(&[control])?;
        if control & CHAR_SAME == 0 {
            put_char(&mut out, cell.c)?;
        }
        if control & FG_CHANGED != 0 {
            encode_color(&mut out, cell.fg)?;
        }
        if control & BG_CHANGED != 0 {
            encode_color(&mut out, cell.bg)?;
        }
        if control & FLAGS_CHANGED != 0 {
            put_u16(&mut out, flags)?;
        }
        if control & UNDERLINE_CHANGED != 0 {
            match underline {
                Some(color) => {
                    out.write_all(&[1])?;
                    encode_color(&mut out, color)?;
                }
                None => out.write_all(&[0])?,
            }
        }
        if control & LINK_CHANGED != 0 {
            put_u16(&mut out, link_index.unwrap_or(u16::MAX))?;
        }
        if control & ZERO_WIDTH_PRESENT != 0 {
            put_u32(
                &mut out,
                u32::try_from(zw.len()).map_err(|_| anyhow!("cell zerowidth count exceeds u32"))?,
            )?;
            for c in zw {
                put_u32(&mut out, *c as u32)?;
            }
        }
        previous_char = cell.c;
        previous_fg = cell.fg;
        previous_bg = cell.bg;
        previous_flags = cell.flags;
        previous_underline = underline;
        previous_link = link_index;
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| anyhow!("row offset overflow"))?;
        ensure!(end <= self.bytes.len(), "truncated row");
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn string(&mut self, pool_budget: &mut usize) -> Result<String> {
        let n = self.u32()? as usize;
        *pool_budget = pool_budget
            .checked_add(n)
            .ok_or_else(|| anyhow!("link byte count overflow"))?;
        ensure!(
            *pool_budget <= MAX_LINK_BYTES,
            "row hyperlink pool exceeds 2 MiB"
        );
        Ok(std::str::from_utf8(self.take(n)?)?.to_owned())
    }
    fn character(&mut self) -> Result<char> {
        let first = self.u8()?;
        let len = match first {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => bail!("invalid UTF-8 scalar prefix"),
        };
        let mut encoded = [0u8; 4];
        encoded[0] = first;
        encoded[1..len].copy_from_slice(self.take(len - 1)?);
        let text = std::str::from_utf8(&encoded[..len])?;
        let mut chars = text.chars();
        let c = chars.next().ok_or_else(|| anyhow!("empty scalar"))?;
        ensure!(chars.next().is_none(), "cell character is not one scalar");
        Ok(c)
    }
    fn character_from_u32(&mut self) -> Result<char> {
        char::from_u32(self.u32()?).ok_or_else(|| anyhow!("invalid Unicode scalar"))
    }
    fn color(&mut self) -> Result<Color> {
        Ok(match self.u8()? {
            0 => Color::Named(named(self.u16()?)?),
            1 => Color::Spec(Rgb {
                r: self.u8()?,
                g: self.u8()?,
                b: self.u8()?,
            }),
            2 => Color::Indexed(self.u8()?),
            _ => bail!("invalid color tag"),
        })
    }
}

fn named(n: u16) -> Result<NamedColor> {
    Ok(match n {
        0 => NamedColor::Black,
        1 => NamedColor::Red,
        2 => NamedColor::Green,
        3 => NamedColor::Yellow,
        4 => NamedColor::Blue,
        5 => NamedColor::Magenta,
        6 => NamedColor::Cyan,
        7 => NamedColor::White,
        8 => NamedColor::BrightBlack,
        9 => NamedColor::BrightRed,
        10 => NamedColor::BrightGreen,
        11 => NamedColor::BrightYellow,
        12 => NamedColor::BrightBlue,
        13 => NamedColor::BrightMagenta,
        14 => NamedColor::BrightCyan,
        15 => NamedColor::BrightWhite,
        256 => NamedColor::Foreground,
        257 => NamedColor::Background,
        258 => NamedColor::Cursor,
        259 => NamedColor::DimBlack,
        260 => NamedColor::DimRed,
        261 => NamedColor::DimGreen,
        262 => NamedColor::DimYellow,
        263 => NamedColor::DimBlue,
        264 => NamedColor::DimMagenta,
        265 => NamedColor::DimCyan,
        266 => NamedColor::DimWhite,
        267 => NamedColor::BrightForeground,
        268 => NamedColor::DimForeground,
        _ => bail!("invalid named color"),
    })
}

pub fn decode_row(bytes: &[u8]) -> Result<DecodedRow> {
    ensure!(bytes.len() <= MAX_ROW_BYTES, "encoded row exceeds 16 MiB");
    let mut r = Reader { bytes, at: 0 };
    ensure!(r.take(8)? == MAGIC, "invalid row magic");
    let columns = r.u16()? as usize;
    let occupied = r.u16()? as usize;
    let link_count = r.u16()? as usize;
    ensure!(
        (1..=MAX_COLUMNS).contains(&columns),
        "row column count outside bounds"
    );
    ensure!(occupied <= columns, "occupied bound exceeds columns");
    ensure!(link_count <= columns, "hyperlink count exceeds columns");
    let mut link_bytes = 0;
    let mut hyperlinks = Vec::with_capacity(link_count);
    for _ in 0..link_count {
        hyperlinks.push(DecodedHyperlink {
            id: r.string(&mut link_bytes)?,
            uri: r.string(&mut link_bytes)?,
        });
    }
    let mut cells = Vec::with_capacity(columns);
    let mut zero_total = 0usize;
    let mut previous_char = ' ';
    let mut fg = Color::Named(NamedColor::Foreground);
    let mut bg = Color::Named(NamedColor::Background);
    let mut flags = Flags::empty();
    let mut underline_color = None;
    let mut hyperlink = None;
    for _ in 0..columns {
        let control = r.u8()?;
        ensure!(control & 0x80 == 0, "reserved cell control bit set");
        let c = if control & CHAR_SAME != 0 {
            previous_char
        } else {
            r.character()?
        };
        if control & FG_CHANGED != 0 {
            fg = r.color()?;
        }
        if control & BG_CHANGED != 0 {
            bg = r.color()?;
        }
        if control & FLAGS_CHANGED != 0 {
            let bits = r.u16()?;
            ensure!(bits & !VALID_FLAGS == 0, "invalid cell flags");
            flags = Flags::from_bits(bits).ok_or_else(|| anyhow!("invalid cell flags"))?;
        }
        if control & UNDERLINE_CHANGED != 0 {
            underline_color = match r.u8()? {
                0 => None,
                1 => Some(r.color()?),
                _ => bail!("invalid underline color marker"),
            };
        }
        if control & LINK_CHANGED != 0 {
            let link = r.u16()?;
            ensure!(
                link == u16::MAX || (link as usize) < hyperlinks.len(),
                "invalid hyperlink reference"
            );
            hyperlink = (link != u16::MAX).then_some(link);
        }
        let mut zerowidth = Vec::new();
        if control & ZERO_WIDTH_PRESENT != 0 {
            let zw_count = usize::try_from(r.u32()?)?;
            zero_total = zero_total
                .checked_add(zw_count)
                .ok_or_else(|| anyhow!("zerowidth count overflow"))?;
            ensure!(
                zero_total <= MAX_ZERO_WIDTH_CHARS,
                "row zerowidth character limit exceeded"
            );
            ensure!(
                zw_count <= r.bytes.len().saturating_sub(r.at) / 4,
                "truncated zerowidth data"
            );
            zerowidth.reserve(zw_count);
            for _ in 0..zw_count {
                zerowidth.push(r.character_from_u32()?);
            }
        }
        cells.push(DecodedCell {
            c,
            fg,
            bg,
            flags,
            zerowidth,
            underline_color,
            hyperlink,
        });
        previous_char = c;
    }
    ensure!(r.at == bytes.len(), "trailing bytes after row");
    Ok(DecodedRow {
        columns: columns as u16,
        occupied: occupied as u16,
        hyperlinks,
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::cell::Cell;

    fn encode(row: &Row<Cell>) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_row(row, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn round_trips_unicode_attributes_occupancy_and_link_pool() {
        let mut cells = vec![Cell::default(); 4];
        cells[0].c = '界';
        cells[0].flags = Flags::WIDE_CHAR | Flags::BOLD | Flags::UNDERLINE;
        cells[0].fg = Color::Spec(Rgb {
            r: 12,
            g: 34,
            b: 56,
        });
        cells[0].bg = Color::Indexed(231);
        cells[0].push_zerowidth('\u{301}');
        cells[0].set_underline_color(Some(Color::Named(NamedColor::BrightCyan)));
        let link = Hyperlink::new(Some("explicit-7"), "https://example.test/path".to_owned());
        cells[0].set_hyperlink(Some(link.clone()));
        cells[1].c = ' ';
        cells[1].flags = Flags::WIDE_CHAR_SPACER | Flags::WRAPLINE;
        cells[1].set_hyperlink(Some(link));
        let row = Row::from_vec(cells, 2);
        let decoded = decode_row(&encode(&row)).unwrap();
        assert_eq!(decoded.columns, 4);
        assert_eq!(decoded.occupied, 2);
        assert_eq!(decoded.cells[0].c, '界');
        assert_eq!(
            decoded.cells[0].fg,
            Color::Spec(Rgb {
                r: 12,
                g: 34,
                b: 56
            })
        );
        assert_eq!(decoded.cells[0].bg, Color::Indexed(231));
        assert_eq!(
            decoded.cells[0].flags,
            Flags::WIDE_CHAR | Flags::BOLD | Flags::UNDERLINE
        );
        assert_eq!(decoded.cells[0].zerowidth, vec!['\u{301}']);
        assert_eq!(
            decoded.cells[0].underline_color,
            Some(Color::Named(NamedColor::BrightCyan))
        );
        assert_eq!(decoded.cells[0].hyperlink, Some(0));
        assert_eq!(decoded.cells[1].hyperlink, Some(0));
        assert_eq!(
            decoded.hyperlinks,
            vec![DecodedHyperlink {
                id: "explicit-7".into(),
                uri: "https://example.test/path".into()
            }]
        );
    }

    #[test]
    fn delta_codec_round_trips_attribute_transitions_and_scalar_widths() {
        let link = Hyperlink::new(Some("delta"), "https://delta.test/".to_owned());
        let cells = vec![
            Cell::default(),
            Cell {
                c: 'a',
                ..Cell::default()
            },
            Cell {
                c: 'a',
                fg: Color::Named(NamedColor::Red),
                bg: Color::Indexed(42),
                flags: Flags::BOLD | Flags::UNDERLINE,
                ..{
                    let mut cell = Cell::default();
                    cell.set_underline_color(Some(Color::Spec(Rgb { r: 1, g: 2, b: 3 })));
                    cell.set_hyperlink(Some(link.clone()));
                    cell
                }
            },
            Cell {
                c: 'a',
                ..Cell::default()
            },
            Cell {
                c: 'é',
                ..Cell::default()
            },
            Cell {
                c: '界',
                ..Cell::default()
            },
            Cell {
                c: '🦀',
                ..Cell::default()
            },
            Cell {
                c: '🦀',
                ..Cell::default()
            },
        ];
        let row = Row::from_vec(cells, 8);
        let decoded = decode_row(&encode(&row)).unwrap();
        assert_eq!(
            decoded.cells.iter().map(|cell| cell.c).collect::<Vec<_>>(),
            vec![' ', 'a', 'a', 'a', 'é', '界', '🦀', '🦀']
        );
        assert_eq!(decoded.cells[2].fg, Color::Named(NamedColor::Red));
        assert_eq!(decoded.cells[2].bg, Color::Indexed(42));
        assert_eq!(decoded.cells[2].flags, Flags::BOLD | Flags::UNDERLINE);
        assert_eq!(
            decoded.cells[2].underline_color,
            Some(Color::Spec(Rgb { r: 1, g: 2, b: 3 }))
        );
        assert_eq!(decoded.cells[2].hyperlink, Some(0));
        assert_eq!(decoded.cells[3].fg, Color::Named(NamedColor::Foreground));
        assert_eq!(decoded.cells[3].bg, Color::Named(NamedColor::Background));
        assert!(decoded.cells[3].flags.is_empty());
        assert_eq!(decoded.cells[3].underline_color, None);
        assert_eq!(decoded.cells[3].hyperlink, None);
        assert_eq!(decoded.hyperlinks.len(), 1);
    }

    #[test]
    fn ascii_row_is_compact() {
        let row = Row::from_vec(
            vec![
                Cell {
                    c: 'x',
                    ..Cell::default()
                };
                120
            ],
            120,
        );
        let bytes = encode(&row);
        assert!(bytes.len() <= 350, "encoded {} bytes", bytes.len());
        // Previous absolute-cell layout was header 14 + 120 * 19 = 2294 bytes.
        assert_eq!(bytes.len(), 135);
    }

    #[test]
    fn round_trips_every_vte_named_color() {
        let named = [
            NamedColor::Black,
            NamedColor::Red,
            NamedColor::Green,
            NamedColor::Yellow,
            NamedColor::Blue,
            NamedColor::Magenta,
            NamedColor::Cyan,
            NamedColor::White,
            NamedColor::BrightBlack,
            NamedColor::BrightRed,
            NamedColor::BrightGreen,
            NamedColor::BrightYellow,
            NamedColor::BrightBlue,
            NamedColor::BrightMagenta,
            NamedColor::BrightCyan,
            NamedColor::BrightWhite,
            NamedColor::Foreground,
            NamedColor::Background,
            NamedColor::Cursor,
            NamedColor::DimBlack,
            NamedColor::DimRed,
            NamedColor::DimGreen,
            NamedColor::DimYellow,
            NamedColor::DimBlue,
            NamedColor::DimMagenta,
            NamedColor::DimCyan,
            NamedColor::DimWhite,
            NamedColor::BrightForeground,
            NamedColor::DimForeground,
        ];
        let cells = named
            .iter()
            .map(|&color| Cell {
                fg: Color::Named(color),
                ..Cell::default()
            })
            .collect();
        let decoded = decode_row(&encode(&Row::from_vec(cells, named.len()))).unwrap();
        assert_eq!(
            decoded.cells.iter().map(|cell| cell.fg).collect::<Vec<_>>(),
            named.map(Color::Named)
        );
    }

    #[test]
    fn shared_large_uri_is_stored_once() {
        let uri = format!("https://example.test/{}", "x".repeat(128 * 1024));
        let link = Hyperlink::new(Some("same"), uri.clone());
        let mut cells = vec![Cell::default(); 128];
        for c in &mut cells {
            c.set_hyperlink(Some(link.clone()));
        }
        let bytes = encode(&Row::from_vec(cells, 128));
        assert!(bytes.len() < uri.len() + 128 * 32);
        let decoded = decode_row(&bytes).unwrap();
        assert_eq!(decoded.hyperlinks.len(), 1);
        assert!(decoded.cells.iter().all(|cell| cell.hyperlink == Some(0)));
    }

    #[test]
    fn rejects_adversarial_geometry_and_invalid_pool_reference() {
        let mut bad_geometry = MAGIC.to_vec();
        bad_geometry.extend_from_slice(&u16::MAX.to_le_bytes());
        bad_geometry.extend_from_slice(&0u16.to_le_bytes());
        bad_geometry.extend_from_slice(&0u16.to_le_bytes());
        assert!(decode_row(&bad_geometry).is_err());

        let mut bad_ref = MAGIC.to_vec();
        bad_ref.extend_from_slice(&1u16.to_le_bytes());
        bad_ref.extend_from_slice(&0u16.to_le_bytes());
        bad_ref.extend_from_slice(&0u16.to_le_bytes());
        bad_ref.push(CHAR_SAME | LINK_CHANGED);
        bad_ref.extend_from_slice(&0u16.to_le_bytes());
        assert!(decode_row(&bad_ref).is_err());

        let mut bad_count = MAGIC.to_vec();
        bad_count.extend_from_slice(&1u16.to_le_bytes());
        bad_count.extend_from_slice(&0u16.to_le_bytes());
        bad_count.extend_from_slice(&0u16.to_le_bytes());
        bad_count.push(CHAR_SAME | ZERO_WIDTH_PRESENT);
        bad_count.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_row(&bad_count).is_err());
    }

    #[test]
    fn zero_width_count_supports_more_than_u16_per_cell() {
        let mut cell = Cell::default();
        for _ in 0..65_536 {
            cell.push_zerowidth('\u{301}');
        }
        let decoded = decode_row(&encode(&Row::from_vec(vec![cell], 1))).unwrap();
        assert_eq!(decoded.cells[0].zerowidth.len(), 65_536);
    }

    #[test]
    fn encoder_rejects_invalid_flag_bits() {
        let cell = Cell {
            flags: Flags::from_bits_retain(0x8000),
            ..Cell::default()
        };
        assert!(encode_row(&Row::from_vec(vec![cell], 1), &mut Vec::new()).is_err());
    }

    #[test]
    fn propagates_writer_failure_and_enforces_zero_width_limit() {
        struct Fails;
        impl Write for Fails {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(encode_row(&Row::from_vec(vec![Cell::default()], 0), &mut Fails).is_err());

        let mut c = Cell::default();
        // The extra is intentionally built incrementally to exercise the aggregate bound.
        for _ in 0..=MAX_ZERO_WIDTH_CHARS {
            c.push_zerowidth('\u{301}');
        }
        assert!(encode_row(&Row::from_vec(vec![c], 1), &mut Vec::new()).is_err());
    }
}
