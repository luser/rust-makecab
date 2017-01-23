//! A simple Microsoft cabinet compressor.
//!
//! Only supports writing a single file to a single folder.
//! Cabinet format structures derived from the [Microsoft Cabinet File Format]
//! documentation.
//!
//! [Microsoft Cabinet File Format]: https://msdn.microsoft.com/en-us/library/bb417343.aspx#cabinet_format

#![allow(non_camel_case_types, non_snake_case)]

#[macro_use]
extern crate error_chain;
extern crate chrono;
extern crate filetime;
extern crate mszip;

use std::fs::File;
use std::io::prelude::*;
use std::io::{self, SeekFrom, BufWriter};
use std::mem;
use std::path::Path;
use std::slice;

use chrono::{Datelike, Local, Timelike, TimeZone};
use filetime::FileTime;
use mszip::{Compression, MSZipEncoder};

mod errors {
    use mszip;

    // Create the Error, ErrorKind, ResultExt, and Result types
    error_chain! {
        errors {
            BadFilename
        }
        links {
            MSZip(mszip::Error, mszip::ErrorKind);
        }
        foreign_links {
            Io(::std::io::Error);
        }
    }
}

use errors::*;

/// Magic number at the start of a cabinet file.
const SIGNATURE: [u8; 4] = [b'M',b'S',b'C',b'F'];
/// File format version, currently 1.3.
const VERSION: (u8, u8) = (1, 3);

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
#[allow(dead_code)]
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

/// Write data from `input` to `output` as `CFDATA` blocks and return the number of blocks written.
fn write_all_cfdata<T: Write, R: Read>(mut output: &mut T, mut compress: &mut MSZipEncoder<R>) -> Result<u16> {
    let mut num_blocks = 0;
    loop {
        match try!(compress.read_block()) {
            ref block if block.data.len() > 0 => {
                let this_block = CFDATA {
                    //TODO: should generate a checksum:
                    // https://msdn.microsoft.com/en-us/library/bb417343.aspx#chksum
                    csum: 0,
                    cbData: block.data.len() as u16,
                    cbUncomp: block.original_size as u16,
                };
                try!(write(&mut output, &this_block));
                try!(output.write_all(block.data));
                num_blocks += 1;
            }
            _ => break,
        }
    }
    Ok(num_blocks)
}

/// Write a cabinet file at `cab_path` containing the single file `input_path`.
pub fn make_cab<T: AsRef<Path>, U: AsRef<Path>>(cab_path: T, input_path: U) -> Result<()> {
    let input = try!(File::open(input_path.as_ref()));
    let input_filename = match input_path.as_ref().file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => bail!(ErrorKind::BadFilename),
    };
    let meta = try!(input.metadata());
    let mtime = FileTime::from_last_modification_time(&meta);
    let mtime = Local.timestamp(mtime.seconds_relative_to_1970() as i64,
                                mtime.nanoseconds());
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
    // If we write more than one file we can reuse the compressor.
    let mut compress = MSZipEncoder::new(input, Compression::Default);
    folder.cCFData = try!(write_all_cfdata(&mut f, &mut compress));
    // Set the file length.
    header.cbCabinet = try!(tell(&mut f)) as u32;
    // Re-write the header and folder entries.
    try!(f.seek(SeekFrom::Start(0)));
    try!(write(&mut f, &header));
    try!(write(&mut f, &folder));
    Ok(())
}

// If I ever add support for extracting files from cabinets, I could
// add round-trip tests for that here, but for now we'll live with
// just testing creating cabinet files and then extracting them with
// `expand`. The API Microsoft exposes for working with cabinet files
// is horrendously complex, so rather than try to wrap that with Rust
// FFI we'll just shell out to `expand`.
#[cfg(all(test, windows))]
mod tests {
    extern crate tempdir;

    use std::fs::File;
    use std::io;
    use std::io::prelude::*;
    use std::process::Command;

    use super::make_cab;
    use self::tempdir::TempDir;

    // Write `data` to a file, create a cabinet file from it, and then
    // extract the file using `expand` and verify that the data is the same.
    fn roundtrip(data: &[u8]) {
        let t = TempDir::new("makecab").expect("failed to create temp dir");
        let in_path = t.path().join("original");
        {
            let mut f = File::create(&in_path).expect("failed to create test file");
            f.write_all(data).expect("failed to write test data");
        }
        let cab = t.path().join("test.cab");
        make_cab(&cab, &in_path).expect("failed to create cab file");

        let out_path = t.path().join("extracted");
        let output = Command::new("expand")
            .arg(&cab)
            .arg(&out_path)
            .output()
            .expect("failed to run expand");
        if output.status.success() {
            let mut buf = vec!();
            {
                File::open(&out_path)
                    .expect("failed to open output file")
                    .read_to_end(&mut buf)
                    .expect("failed to read output file");
            }
            assert_eq!(data, &buf[..]);
        } else {
            writeln!(io::stderr(), "Error running expand.
Its stdout was:
=====================
{}
=====================

Its stderr was:
=====================
{}
=====================
",
                                        String::from_utf8_lossy(&output.stdout),
                                        String::from_utf8_lossy(&output.stderr)).unwrap();
            assert!(false);
        }
    }

    /// Generate a `Vec<u8>` of test data of `size` bytes.
    fn test_data(size: usize) -> Vec<u8> {
        (0..size).map(|v| (v % (u8::max_value() as usize + 1)) as u8).collect::<Vec<u8>>()
    }

    macro_rules! t {
        ($name:ident, $e:expr) => {
            #[test]
            fn $name() {
                let data = $e;
                roundtrip(&data[..]);
            }
        }
    }

    const MAX_CHUNK: usize = 32 * 1024;

    t!(zeroes, vec![0; 1000]);
    t!(nonzero_many_blocks, test_data(MAX_CHUNK * 8));
    t!(firefox_exe, include_bytes!("../testdata/firefox.exe"));
}
