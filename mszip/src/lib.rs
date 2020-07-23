#[macro_use]
extern crate error_chain;
extern crate flate2;
extern crate libc;
extern crate libz_sys;

use std::io::prelude::*;
use std::io::{self, BufReader, Cursor};

use flate2::{Compress, Decompress, Flush, Status};
use flate2::raw::mz_stream;
pub use flate2::Compression;

use libz_sys::{Z_OK, inflateReset, inflateSetDictionary};

// libz-sys doesn't expose these, as they're undocumented and not
// generally useful, but per Mark Adler they're what we want:
// http://stackoverflow.com/a/39392530/69326
#[cfg(all(target_env = "msvc", target_pointer_width = "32"))]
extern {
    fn deflateResetKeep(strm: *mut mz_stream) -> libc::c_int;
    // This doesn't seem to actually work right.
    //fn inflateResetKeep(strm: *mut mz_stream) -> libc::c_int;
}

#[cfg(not(all(target_env = "msvc", target_pointer_width = "32")))]
extern "system" {
    fn deflateResetKeep(strm: *mut mz_stream) -> libc::c_int;
    //fn inflateResetKeep(strm: *mut mz_stream) -> libc::c_int;
}

mod errors {
    // Create the Error, ErrorKind, ResultExt, and Result types
    error_chain! {
        errors {
            BlockSizeTooLarge
            InvalidBlockSignature
            BufferError
            DecompressionError
        }
        foreign_links {
            Io(::std::io::Error);
            Flate(::flate2::DataError);
        }
    }
}

pub use errors::*;

/// Magic number at the start of MS-ZIP compressed data.
const MSZIP_SIGNATURE: [u8; 2] = [b'C', b'K'];
const SIG_LEN: usize = 2;
/// Maximum uncompressed bytes in an input chunk: 32KB.
pub const MAX_CHUNK: usize = 32768;
/// The maximum size of an MSZIP compressed block: 32KB + 12 bytes.
pub const MAX_BLOCK_SIZE: usize = MAX_CHUNK + 12;

fn run_compress<R: BufRead>(obj: &mut R, data: &mut Compress, mut dst: &mut [u8]) -> io::Result<(usize, usize)> {
    // Cribbed from flate2-rs. Wish this was public API!
    let mut total_read = 0;
    let mut total_consumed = 0;
    loop {
        let (read, consumed, ret, eof);
        {
            let input = obj.fill_buf()?;
            eof = input.is_empty();
            let before_out = data.total_out();
            let before_in = data.total_in();
            let flush = if eof {Flush::Finish} else {Flush::None};
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

/// A single MSZIP block of compressed data.
pub struct MSZipBlock<'a> {
    /// The original size of the input data that was compressed.
    pub original_size: usize,
    /// The compressed data.
    pub data: &'a [u8],
}

/// An MSZIP compressor.
///
/// This structure will read data from an underlying `Read` stream and
/// produce MSZIP-compressed blocks.
pub struct MSZipEncoder<R: Read> {
    reader: BufReader<R>,
    compress: Compress,
    out_buffer: Vec<u8>,
}

impl<R: Read> MSZipEncoder<R> {
    /// Creates a new encoder which will read uncompressed data from `reader`
    /// and emit compressed blocks when `read_block` is called.
    pub fn new(reader: R, level: Compression) -> MSZipEncoder<R> {
        MSZipEncoder {
            reader: BufReader::with_capacity(MAX_CHUNK, reader),
            compress: Compress::new(level, false),
            out_buffer: vec![0; MAX_BLOCK_SIZE],
        }
    }

    /// Reads a single MSZIP block of compressed data.
    pub fn read_block<'a>(&'a mut self) -> Result<MSZipBlock<'a>> {
        let (read, written) = {
            let mut chunk = Cursor::new(self.reader.fill_buf()?);
            let nbytes = chunk.get_ref().len();
            if nbytes == 0 {
                return Ok(MSZipBlock {
                    original_size: 0,
                    data: &self.out_buffer[..0],
                });
            }
            // Prepend the MS-ZIP signature to each chunk.
            &self.out_buffer[..SIG_LEN].copy_from_slice(&MSZIP_SIGNATURE);
            run_compress(&mut chunk, &mut self.compress, &mut self.out_buffer[SIG_LEN..])?
        };
        self.reader.consume(read);
        unsafe {
            // Reset the deflate compressor for the next block, since we've
            // asked it to `Flush::Finish`, but keep its compression dictionary.
            assert_eq!(Z_OK, deflateResetKeep(self.compress.get_raw()));
        }
        return Ok(MSZipBlock {
            original_size: read,
            data: &self.out_buffer[..written+SIG_LEN],
        });
    }
}

/// An MSZIP decompressor.
///
/// When MSZIP-compressed blocks are passed to this structure's `write_block`
/// method, the data will be decompressed and written to the underlying
/// `Write` stream.
pub struct MSZipDecoder<W: Write> {
    writer: W,
    decompress: Decompress,
    buffer: Vec<u8>,
}

impl<W: Write> MSZipDecoder<W> {
    /// Creates a new decoder which will write uncompressed data to `writer`.
    pub fn new(writer: W) -> MSZipDecoder<W> {
        MSZipDecoder {
            writer: writer,
            decompress: Decompress::new(false),
            buffer: vec![0; MAX_CHUNK],
        }
    }

    /// Decompresses the single MSZIP block `block`.
    pub fn write_block(&mut self, block: &[u8]) -> Result<()> {
        if block.len() > MAX_BLOCK_SIZE {
            bail!(ErrorKind::BlockSizeTooLarge);
        }
        if &block[..MSZIP_SIGNATURE.len()] != MSZIP_SIGNATURE {
            bail!(ErrorKind::InvalidBlockSignature);
        }
        println!("trying to decompress {} bytes", block.len() - MSZIP_SIGNATURE.len());
        let last = self.decompress.total_out();
        match self.decompress.decompress(&block[MSZIP_SIGNATURE.len()..],
                                         &mut self.buffer,
                                         Flush::Finish)? {
            Status::StreamEnd => {
                let written = (self.decompress.total_out() - last) as usize;
                println!("writing {} bytes to writer", written);
                let decompressed = &self.buffer[..written];
                self.writer.write_all(decompressed)?;
                unsafe {
                    // Reset the decompressor for the next block, but keep
                    // its decompression dictionary.
                    // We should use this, but either it doesn't work or
                    // I'm not using it right.
                    //assert_eq!(Z_OK, inflateResetKeep(self.decompress.get_raw()));
                    assert_eq!(Z_OK, inflateReset(self.decompress.get_raw()));
                    assert_eq!(Z_OK, inflateSetDictionary(self.decompress.get_raw(), decompressed.as_ptr(), decompressed.len() as u32));
                }
            }
            Status::Ok => bail!(ErrorKind::DecompressionError),
            Status::BufError => bail!(ErrorKind::BufferError),
        }
        Ok(())
    }

    /// Returns the underlying writer.
    pub fn finish(self) -> Result<W> {
        Ok(self.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::u8;

    #[macro_use]
    #[cfg(windows)]
    /// Wrappers for the Microsoft compression API so that on Windows we
    /// can test interop with the system implementation.
    mod sys {
        #![allow(non_camel_case_types)]
        extern crate winapi;

        use super::super::*;
        use std::io::Cursor;
        use std::mem;
        use std::ptr;
        use std::result;
        use self::winapi::minwindef::{BOOL, DWORD, LPVOID, TRUE, FALSE};
        use self::winapi::winnt::{HANDLE, PVOID};
        use self::winapi::basetsd::{SIZE_T, PSIZE_T};

        const COMPRESS_ALGORITHM_MSZIP: DWORD = 2;
        const COMPRESS_RAW: DWORD = 1 << 29;
        type PCOMPRESS_ALLOCATION_ROUTINES = LPVOID;
        type COMPRESSOR_HANDLE = HANDLE;
        type DECOMPRESSOR_HANDLE = HANDLE;
        type PCOMPRESSOR_HANDLE = *mut COMPRESSOR_HANDLE;
        type PDECOMPRESSOR_HANDLE = *mut DECOMPRESSOR_HANDLE;

        #[link(name = "cabinet")]
        extern "system" {
            fn CreateCompressor(Algorithm: DWORD,
                                AllocationRoutines: LPVOID,
                                CompressorHandle: PCOMPRESSOR_HANDLE) -> BOOL;
            fn CloseCompressor(CompressorHandle: COMPRESSOR_HANDLE) -> BOOL;
            fn Compress(CompressorHandle: COMPRESSOR_HANDLE,
                        UncompressedData: PVOID,
                        UncompressedDataSize: SIZE_T,
                        CompressedBuffer: PVOID,
                        CompressedBufferSize: SIZE_T,
                        CompressedDataSize: PSIZE_T) -> BOOL;

            fn CreateDecompressor(Algorithm: DWORD,
                                  AllocationRoutines: PCOMPRESS_ALLOCATION_ROUTINES,
                                  DecompressorHandle: PDECOMPRESSOR_HANDLE) -> BOOL;
            fn CloseDecompressor(DecompressorHandle: DECOMPRESSOR_HANDLE) -> BOOL;
            fn Decompress(DecompressorHandle: DECOMPRESSOR_HANDLE,
                          CompressedData: PVOID,
                          CompressedDataSize: SIZE_T,
                          UncompressedBuffer: PVOID,
                          UncompressedBufferSize: SIZE_T,
                          UncompressedDataSize: PSIZE_T) -> BOOL;
        }

        /// Compress `data` with the Microsoft compression API.
        fn do_system_compress(data: &[u8]) -> result::Result<Vec<Vec<u8>>, &'static str> {
            let h = unsafe {
                let mut h: COMPRESSOR_HANDLE = mem::uninitialized();
                if CreateCompressor(COMPRESS_ALGORITHM_MSZIP | COMPRESS_RAW, ptr::null_mut(), &mut h as PCOMPRESSOR_HANDLE) == TRUE {
                    h
                } else {
                    return Err("CreateCompressor failed");
                }
            };
            // Result is a vec of blocks, each a vec of bytes.
            let mut result = vec!();
            for chunk in data.chunks(MAX_CHUNK) {
                // Allocate compression buffer.
                let mut buf = vec![0; MAX_BLOCK_SIZE];
                // Run compression
                unsafe {
                    let mut compressed_size: SIZE_T = mem::uninitialized();
                    if Compress(h, chunk.as_ptr() as PVOID, chunk.len() as SIZE_T, buf.as_ptr() as PVOID, buf.len() as SIZE_T, &mut compressed_size as PSIZE_T) == FALSE {
                        return Err("Compress failed");
                    }
                    buf.resize(compressed_size as usize, 0);
                }
                result.push(buf);
            }
            unsafe { CloseCompressor(h); }
            Ok(result)
        }

        /// Decompress `chunks` through the Microsoft compression API.
        fn do_system_decompress(chunks: &Vec<(usize, Vec<u8>)>) -> result::Result<Vec<u8>, &'static str> {
            let h = unsafe {
                let mut h: DECOMPRESSOR_HANDLE = mem::uninitialized();
                if CreateDecompressor(COMPRESS_ALGORITHM_MSZIP | COMPRESS_RAW, ptr::null_mut(), &mut h as PDECOMPRESSOR_HANDLE) == TRUE {
                    h
                } else {
                    return Err("CreateDecompressor failed");
                }
            };
            let mut buf = vec!();
            // Decompress each chunk in turn..
            for &(original_size, ref chunk) in chunks.iter() {
                assert!(original_size <= MAX_CHUNK);
                // Make space in the output buffer.
                let last = buf.len();
                buf.resize(last + original_size, 0);
                unsafe {
                    if Decompress(h, chunk.as_ptr() as PVOID, chunk.len() as SIZE_T, buf[last..].as_mut_ptr() as PVOID, original_size as SIZE_T, ptr::null_mut()) == FALSE {
                        return Err("Decompress failed");
                    }
                }
            }
            unsafe { CloseDecompressor(h) };
            Ok(buf)
        }

        /// Run `data` through `MSZipEncoder` and then the system decompressor
        /// and assert that the end result is the same.
        pub fn roundtrip_compress_system_decompressor(data: &[u8]) {
            let mut compress = MSZipEncoder::new(Cursor::new(data), Compression::Default);
            let mut chunks = vec!();
            loop {
                match compress.read_block() {
                    Ok(ref block) if block.data.len() > 0 => chunks.push((block.original_size, block.data.to_owned())),
                    _ => break,
                }
            }
            let decompressed = do_system_decompress(&chunks).unwrap();
            assert_eq!(data, &decompressed[..]);
        }

        /// Run `data` through the system compressor and then `MSZipDecoder`
        /// and assert that the end result is the same.
        pub fn roundtrip_system_compressor_decompress(data: &[u8]) {
            let chunks = do_system_compress(&data).unwrap();
            println!("compressed to {} chunks", chunks.len());
            let mut decompress = MSZipDecoder::new(Cursor::new(vec!()));
            for chunk in chunks {
                decompress.write_block(&chunk).unwrap();
            }
            let decompressed = decompress.finish().unwrap().into_inner();
            assert_eq!(data, &decompressed[..]);
        }

        // Insert tests for round-tripping through the system compressor
        // and decompressor.
        macro_rules! sys_tests {
            ($e:expr) => {
                use super::sys::{roundtrip_compress_system_decompressor, roundtrip_system_compressor_decompress};

                #[test]
                fn test_roundtrip_system_decompressor() {
                    let data = $e;
                    roundtrip_compress_system_decompressor(&data);
                }

                #[test]
                fn test_roundtrip_system_compressor() {
                    let data = $e;
                    roundtrip_system_compressor_decompress(&data);
                }
            }
        }
    }

    #[macro_use]
    #[cfg(not(windows))]
    mod sys {
        macro_rules! sys_tests {
            ($e:expr) => {}
        }
    }

    // Make it easy to run the same tests on different data.
    macro_rules! t {
        ($name:ident, $e:expr) => {
            mod $name {
                #![allow(unused_imports)]
                use super::super::MAX_CHUNK;
                use super::{roundtrip_compress, test_data};

                #[test]
                fn test_roundtrip_compress() {
                    let data = $e;
                    roundtrip_compress(&data);
                }

                sys_tests!($e);
            }
        }
    }

    /// Run `data` through `MSZipEncoder` and `MSZipDecoder` in sequence
    /// and assert that the end result is the same.
    fn roundtrip_compress(data: &[u8]) {
        let mut compress = MSZipEncoder::new(Cursor::new(data), Compression::Default);
        let mut decompress = MSZipDecoder::new(Cursor::new(vec!()));
        loop {
            match compress.read_block() {
                Ok(ref block) if block.data.len() > 0 => decompress.write_block(block.data).unwrap(),
                _ => break,
            }
        }
        match decompress.finish() {
            Ok(result) => {
                let result = result.into_inner();
                assert_eq!(data, &result[..]);
            }
            Err(e) => assert!(false, "Failed to finish decompression: {}", e),
        }
    }

    /// Generate a `Vec<u8>` of test data of `size` bytes.
    pub fn test_data(size: usize) -> Vec<u8> {
        (0..size).map(|v| (v % (u8::max_value() as usize + 1)) as u8).collect::<Vec<u8>>()
    }

    t!(zeroes, vec![0; 1000]);
    t!(zeroes_two_blocks, vec![0; MAX_CHUNK + 1000]);
    t!(exactly_one_block, test_data(MAX_CHUNK));
    t!(one_block_less_a_byte, test_data(MAX_CHUNK - 1));
    t!(one_block_plus_a_byte, test_data(MAX_CHUNK + 1));
    t!(nonzero, test_data(1000));
    t!(nonzero_two_blocks, test_data(MAX_CHUNK + 1000));
    t!(nonzero_many_blocks, test_data(MAX_CHUNK * 8));
}
