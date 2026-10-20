fn main() {
    let root = std::env::args().nth(1).expect("usage: paths STICK_ROOT");
    let stick = nullamp_rekordbox::Stick::open(std::path::Path::new(&root)).expect("open");
    for t in &stick.tracks {
        let s = t.file.to_string_lossy();
        println!("{:?} exists={} non-ascii={:x?}", t.file.file_name().unwrap(), t.file.exists(), s.chars().filter(|c| !c.is_ascii()).collect::<Vec<_>>());
    }
}
