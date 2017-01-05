//! A simple Microsoft cabinet compressor.
//!
//! Only supports writing a single file to a single folder.
//! Cabinet format structures derived from the [Microsoft Cabinet File Format]
//! documentation.
//!
//! [Microsoft Cabinet File Format]: https://msdn.microsoft.com/en-us/library/bb417343.aspx#cabinet_format

#![allow(non_camel_case_types, unused_variables, non_snake_case, dead_code)]

extern crate chrono;
extern crate filetime;
extern crate flate2;

use std::fs::File;
use std::io::prelude::*;
use std::io::{self, Cursor, SeekFrom, BufReader, BufWriter};
use std::mem;
use std::path::Path;
use std::ptr;
use std::slice;
use std::str;

use chrono::{Datelike, Local, Timelike, TimeZone};
use filetime::FileTime;
use flate2::{Compress, Compression, Flush, Status};

/// Magic number at the start of a cabinet file.
const SIGNATURE: [u8; 4] = [b'M',b'S',b'C',b'F'];
/// File format version, currently 1.3.
const VERSION: (u8, u8) = (1, 3);
/// Magic number at the start of MS-ZIP compressed data.
const MSZIP_SIGNATURE: [u8; 2] = [b'C', b'K'];
/// Maximum bytes in a single CFDATA.
const MAX_CHUNK: usize = 32768;

#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
struct CFHEADER {
    signature: [u8; 4],
    reserved1: u32,     /* reserved */
    cbCabinet: u32,     /* size of this cabinet file in bytes */
    reserved2: u32,     /* reserved */
    coffFiles: u32,     /* offset of the first CFFILE entry */
    reserved3: u32,     /* reserved */
    versionMinor: u8,   /* cabinet file format version, minor */
    versionMajor: u8,   /* cabinet file format version, major */
    cFolders: u16,      /* number of CFFOLDER entries in this */
    /*    cabinet */
    cFiles: u16,        /* number of CFFILE entries in this cabinet */
    flags: u16,         /* cabinet file option indicators */
    setID: u16,         /* must be the same for all cabinets in a */
    /*    set */
    iCabinet: u16,      /* number of this cabinet file in a set */

    // These fields are all optional.
    /*
    cbCFHeader: u16,    /* (optional) size of per-cabinet reserved */
    /*    area */
    cbCFFolder: u8,     /* (optional) size of per-folder reserved */
    /*    area */
    cbCFData: u8,       /* (optional) size of per-datablock reserved */
    /*    area */
    abReserve[];: u8,   /* (optional) per-cabinet reserved area */
    szCabinetPrev[]: u8,/* (optional) name of previous cabinet file */
    szDiskPrev[]: u8,   /* (optional) name of previous disk */
    szCabinetNext[]: u8,/* (optional) name of next cabinet file */
    szDiskNext[]: u8,   /* (optional) name of next disk */
     */
}

#[repr(u16)]
#[derive(Copy, Clone, Debug)]
enum CompressionType {
    None  = 0,
    MsZip = 1,
}

#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
struct CFFOLDER {
    coffCabStart: u32,  /* offset of the first CFDATA block in this */
    /*    folder */
    cCFData: u16,       /* number of CFDATA blocks in this folder */
    typeCompress: CompressionType,  /* compression type indicator */
    /*
    abReserve: u1,   /* (optional) per-folder reserved area */
     */
}

#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
struct CFFILE {
    cbFile: u32,          /* uncompressed size of this file in bytes */
    uoffFolderStart: u32, /* uncompressed offset of this file in the folder */
    iFolder: u16,         /* index into the CFFOLDER area */
    date: u16,            /* date stamp for this file */
    time: u16,            /* time stamp for this file */
    attribs: u16,         /* attribute flags for this file */
    /*
    szName[]: u1,         /* name of this file */
     */
}

#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
struct CFDATA {
    csum: u32,         /* checksum of this CFDATA entry */
    cbData: u16,       /* number of compressed bytes in this block */
    cbUncomp: u16,     /* number of uncompressed bytes in this block */
    /*
    abReserve: u8,  /* (optional) per-datablock reserved area */
    ab[cbData]: u8,   /* compressed data bytes */
     */
}

/// Read `count` bytes from `f` and return a `Vec<u8>` of them.
fn read_bytes<T: Read>(f: &mut T, count: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(count);
    try!(f.take(count as u64).read_to_end(&mut buf));
    Ok(buf)
}

/// Convert `bytes` into `T`.
//FIXME: this should be replaced with something based on serialize.
fn transmogrify<T: Copy + Sized>(bytes: &[u8]) -> T {
    assert_eq!(mem::size_of::<T>(), bytes.len());
    unsafe {
        let mut val : T = mem::uninitialized();
        ptr::copy(bytes.as_ptr(), &mut val as *mut T as *mut u8, bytes.len());
        val
    }
}

/// Read a `T` from `f`.
fn read<T: Copy + Sized, U : Read>(f: &mut U) -> io::Result<T> {
    let size = mem::size_of::<T>();
    let buf = try!(read_bytes(f, size));
    Ok(transmogrify::<T>(&buf[..]))
}

/// Write a `T` to `f`.
fn write<T: Sized, U: Write>(f: &mut U, data: &T) -> io::Result<()> {
    let p: *const T = data;
    let p: *const u8 = p as *const u8;
    let s: &[u8] = unsafe {
        slice::from_raw_parts(p, mem::size_of::<T>())
    };
    try!(f.write_all(s));
    Ok(())
}

fn tell<T: Seek>(f: &mut T) -> io::Result<u64> {
    f.seek(SeekFrom::Current(0))
}

fn run_compress<R: BufRead>(obj: &mut R, data: &mut Compress, mut dst: &mut [u8], last: bool) -> io::Result<(usize, usize)> {
    // Cribbed from flate2-rs. Wish this was public API!
    let mut total_read = 0;
    let mut total_consumed = 0;
    loop {
        let (read, consumed, ret, eof);
        {
            let input = try!(obj.fill_buf());
            eof = input.is_empty();
            let before_out = data.total_out();
            let before_in = data.total_in();
            let flush = if last {Flush::Finish} else {Flush::Sync};
            ret = data.compress(input, &mut dst, flush);
            read = (data.total_out() - before_out) as usize;
            consumed = (data.total_in() - before_in) as usize;
        }
        obj.consume(consumed);
        total_consumed += consumed;
        total_read += read;

        match ret {
            // If we haven't ready any data and we haven't hit EOF yet,
            // then we need to keep asking for more data because if we
            // return that 0 bytes of data have been read then it will
            // be interpreted as EOF.
            Status::Ok |
            Status::BufError if read == 0 && !eof && dst.len() > 0 => {
                continue
            }
            Status::Ok |
            Status::BufError |
            Status::StreamEnd => return Ok((total_consumed, total_read)),
        }
    }
}

/// Write data from `input` to `output` as `CFDATA` blocks and return the number of blocks written.
fn write_all_cfdata<T: Write, U: BufRead>(mut output: &mut T, input: &mut U) -> io::Result<u16> {
    let mut compress = Compress::new(Compression::Default, false);
    let mut num_blocks = 0;
    let mut out_buf: [u8; MAX_CHUNK] = [0; MAX_CHUNK];
    loop {
        let (read, written) = {
            let mut chunk = Cursor::new(try!(input.fill_buf()));
            let nbytes = chunk.get_ref().len();
            // Prepend the MS-ZIP signature to each chunk.
            &out_buf[..MSZIP_SIGNATURE.len()].copy_from_slice(&MSZIP_SIGNATURE);
            try!(run_compress(&mut chunk, &mut compress, &mut out_buf[MSZIP_SIGNATURE.len()..], nbytes < MAX_CHUNK))
        };
        input.consume(read);
        if written == 0 {
            break;
        }
        let this_block = CFDATA {
            //TODO: should generate a checksum:
            // https://msdn.microsoft.com/en-us/library/bb417343.aspx#chksum
            csum: 0,
            cbData: (written + MSZIP_SIGNATURE.len()) as u16,
            cbUncomp: read as u16,
        };
        try!(write(&mut output, &this_block));
        try!(output.write_all(&out_buf[..this_block.cbData as usize]));
        num_blocks += 1;
    }
    Ok(num_blocks)
}

/// Write a cabinet file at `cab_path` containing the single file `input_path`.
pub fn make_cab<T: AsRef<Path>, U: AsRef<Path>>(cab_path: T, input_path: U) -> io::Result<()> {
    let input = try!(File::open(input_path.as_ref()));
    let input_filename = match input_path.as_ref().file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                                      "Bad input filename"));
        }
    };
    let meta = try!(input.metadata());
    let mtime = FileTime::from_last_modification_time(&meta);
    let mtime = Local.timestamp(mtime.seconds_relative_to_1970() as i64,
                                mtime.nanoseconds());
    let mut input = BufReader::with_capacity(MAX_CHUNK, input);
    let mut f = BufWriter::new(try!(File::create(cab_path)));
    // Write the header, we'll have to go back and write it again once we know
    // the full size of the file.
    let mut header = CFHEADER {
        signature: SIGNATURE,
        reserved1: 0,
        // This will get set after writing all data.
        cbCabinet: 0,
        reserved2: 0,
        // This will get set after writing the CFFOLDER.
        coffFiles: 0,
        reserved3: 0,
        versionMinor: VERSION.1,
        versionMajor: VERSION.0,
        // We don't support writing more than one folder or file.
        cFolders: 1,
        cFiles: 1,
        flags: 0,
        // We don't support writing multi-cabinet sets.
        setID: 0,
        iCabinet: 0,
    };
    try!(write(&mut f, &header));
    // Write a folder entry.
    let mut folder = CFFOLDER {
        // This will get filled in after we write the CFFILE.
        coffCabStart: 0,
        // This will get filled in after we write all the CFDATA blocks.
        cCFData: 0,
        typeCompress: CompressionType::MsZip,
    };
    try!(write(&mut f, &folder));
    header.coffFiles = try!(tell(&mut f)) as u32;
    // Write a file entry.
    let file = CFFILE {
        cbFile: meta.len() as u32,
        uoffFolderStart: 0,
        iFolder: 0,
        date: ((mtime.year() as u16 - 1980) << 9) + ((mtime.month() as u16) << 5) + mtime.day() as u16,
        time: ((mtime.hour() << 11) + (mtime.minute() << 5) + (mtime.second() / 2)) as u16,
        //TODO: could read these out of meta, but we'll just put ARCHIVE
        // here for now.
        attribs: 0x20,
    };
    try!(write(&mut f, &file));
    // Write filename as a nul-terminated string.
    try!(f.write_all(input_filename.as_bytes()));
    try!(f.write_all(&[0]));
    folder.coffCabStart = try!(tell(&mut f)) as u32;
    folder.cCFData = try!(write_all_cfdata(&mut f, &mut input));
    // Set the file length.
    header.cbCabinet = try!(tell(&mut f)) as u32;
    // Re-write the header and folder entries.
    try!(f.seek(SeekFrom::Start(0)));
    try!(write(&mut f, &header));
    try!(write(&mut f, &folder));
    Ok(())
}
