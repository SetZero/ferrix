use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

fn elf_end(b: &[u8]) -> Option<u64> {
    if b.len() < 64 || &b[..4] != b"\x7fELF" {
        return None;
    }
    let shoff = u64::from_le_bytes(b[0x28..0x30].try_into().ok()?);
    let shentsize = u64::from(u16::from_le_bytes(b[0x3a..0x3c].try_into().ok()?));
    let shnum = u64::from(u16::from_le_bytes(b[0x3c..0x3e].try_into().ok()?));
    Some(shoff + shentsize * shnum)
}

fn check(path: &str) {
    let mut f = std::fs::File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let mut read = Vec::new();
    f.read_to_end(&mut read).unwrap();
    let mapped: Vec<u8> = if len > 0 {
        unsafe {
            let p = mmap(std::ptr::null_mut(), len as usize, 1, 2, f.as_raw_fd(), 0);
            if p as isize == -1 {
                println!("probe {path}: mmap failed");
                Vec::new()
            } else {
                std::slice::from_raw_parts(p, len as usize).to_vec()
            }
        }
    } else {
        Vec::new()
    };
    let first_diff = mapped.iter().zip(&read).position(|(a, b)| a != b);
    println!(
        "probe {path}: stat {len} read {} elf-end(read) {:?} elf-end(mmap) {:?} mmap==read {} first-diff {:?}",
        read.len(),
        elf_end(&read),
        elf_end(&mapped),
        mapped == read,
        first_diff
    );
}

fn mimic(path: &str) {
    let data: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(path).unwrap();
        f.write_all(&data[..40_000]).unwrap();
        f.write_all(&data[40_000..]).unwrap();
        f.seek(SeekFrom::Start(0x28)).unwrap();
        f.write_all(&[1; 8]).unwrap();
        f.seek(SeekFrom::Start(70_000)).unwrap();
    }
    let mut want = data.clone();
    want[0x28..0x30].fill(1);
    let got = std::fs::read(path).unwrap();
    println!(
        "probe mimic {path}: stat {} read {} equal {}",
        std::fs::metadata(path).unwrap().len(),
        got.len(),
        got == want
    );
    check(path);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mimic") {
        mimic(&args[2]);
    } else {
        for path in &args[1..] {
            check(path);
        }
    }
}
