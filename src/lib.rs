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
extern crate cab;
extern crate chrono;
extern crate filetime;

use std::fs::File;
use std::io;
use std::path::Path;

use cab::{CabinetBuilder, CompressionType};
use chrono::NaiveDateTime;
use filetime::FileTime;

mod errors {
    // Create the Error, ErrorKind, ResultExt, and Result types
    error_chain! {
        errors {
            BadFilename
        }
        foreign_links {
            Io(::std::io::Error);
        }
    }
}

use errors::*;

/// Write a cabinet file at `cab_path` containing the single file `input_path`.
pub fn make_cab<T: AsRef<Path>, U: AsRef<Path>>(cab_path: T, input_path: U) -> Result<()> {
    let mut input = File::open(input_path.as_ref())?;
    let input_filename = match input_path.as_ref().file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => bail!(ErrorKind::BadFilename),
    };
    let meta = input.metadata()?;
    let mtime = FileTime::from_last_modification_time(&meta);
    let mtime = NaiveDateTime::from_timestamp(mtime.unix_seconds(), mtime.nanoseconds());
    let mut cab_builder = CabinetBuilder::new();
    let folder = cab_builder.add_folder(CompressionType::MsZip);
    let file = folder.add_file(input_filename);
    file.set_datetime(mtime);

    let cab_file = File::create(cab_path)?;
    let mut cab_writer = cab_builder.build(cab_file)?;
    while let Some(mut writer) = cab_writer.next_file()? {
        io::copy(&mut input, &mut writer)?;
    }
    cab_writer.finish()?;
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

    use self::tempdir::TempDir;
    use super::make_cab;

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
            let mut buf = vec![];
            {
                File::open(&out_path)
                    .expect("failed to open output file")
                    .read_to_end(&mut buf)
                    .expect("failed to read output file");
            }
            assert_eq!(data, &buf[..]);
        } else {
            writeln!(
                io::stderr(),
                "Error running expand.
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
                String::from_utf8_lossy(&output.stderr)
            )
            .unwrap();
            assert!(false);
        }
    }

    /// Generate a `Vec<u8>` of test data of `size` bytes.
    fn test_data(size: usize) -> Vec<u8> {
        (0..size)
            .map(|v| (v % (u8::max_value() as usize + 1)) as u8)
            .collect::<Vec<u8>>()
    }

    macro_rules! t {
        ($name:ident, $e:expr) => {
            #[test]
            fn $name() {
                let data = $e;
                roundtrip(&data[..]);
            }
        };
    }

    const MAX_CHUNK: usize = 32 * 1024;

    t!(zeroes, vec![0; 1000]);
    t!(nonzero_many_blocks, test_data(MAX_CHUNK * 8));
    t!(firefox_exe, include_bytes!("../testdata/firefox.exe"));
}
