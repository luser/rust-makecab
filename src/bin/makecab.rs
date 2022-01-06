//! Create a cabinet file.

use clap::{arg, App};
use std::borrow::Cow;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process;

fn main() {
    let matches = App::new("makecab")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Ted Mielczarek <ted@mielczarek.org>")
        .about("Cabinet Maker (less-fully-featured Rust port)")
        .args(&[
            arg!(-F [directives]        "Not supported"),
            clap::Arg::new("define")
                .short('D')
                .value_name("VAR=VAL")
                .help("Defines variable with specified value."),
            arg!(-L [DIR]               "Location to place destination (default is current directory)"),
            arg!(-V[n]                  "Verbosity level"),
            arg!(<source>               "File to compress")
                .allow_invalid_utf8(true),
            arg!([destination]          "File name to give compressed file. If omitted, the last character of the source file name is replaced with an underscore (_) and used as the destination.")
                .allow_invalid_utf8(true),
            ]
        )
        .get_matches();

    // Check for unsupported options.
    if matches.is_present("directives") {
        println!("Error: directive files are not supported");
        process::exit(1);
    }
    if matches
        .values_of("define")
        .map(|mut vals| vals.any(|v| v != "CompressionType=MSZIP"))
        .unwrap_or(false)
    {
        println!("Error: only '-D CompressionType=MSZIP' is supported.");
        process::exit(1);
    }

    let source = matches.value_of_os("source").unwrap();
    let dest_name = matches
        .value_of_os("destination")
        .map(Cow::Borrowed)
        .unwrap_or_else(|| {
            let s = Path::new(source).file_name().unwrap().to_str().unwrap();
            Cow::Owned(OsString::from(
                s.chars()
                    .take(s.len() - 1)
                    .chain("_".chars())
                    .collect::<String>(),
            ))
        });
    let dest = matches
        .value_of_os("DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap())
        .join(dest_name);
    println!("{:?} -> {:?}", source, dest);
    match makecab::make_cab(dest, source) {
        Ok(()) => {}
        Err(e) => {
            println!("Failed to write cab file: {}", e);
            ::std::process::exit(1);
        }
    }
}
