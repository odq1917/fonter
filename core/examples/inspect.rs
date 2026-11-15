//! Prints the faces and coverage size of the given font files.
//! Set FONTER_RANGES=1 to also print the covered ranges.
use fonter_core::parse_file;
fn main() {
    let ranges = std::env::var_os("FONTER_RANGES").is_some();
    for arg in std::env::args().skip(1) {
        for f in parse_file(std::path::Path::new(&arg)) {
            println!("{}\t{}\t{}\t{}\t{}\t{}", arg, f.index, f.family, f.style, f.cp_count, f.ranges.len());
            if ranges {
                let s: Vec<String> = f.ranges.iter().map(|(a, b)| format!("{:X}-{:X}", a, b)).collect();
                println!("{}", s.join(" "));
            }
        }
    }
}
